# 03. Провайдери сенсорів для user-space таскбар-монітора: shared memory, WMI, HTTP

**Дата дослідження:** 2026-11-17
**Контекст:** Rust-застосунок працює повністю в user space, без власного kernel driver. Температури CPU/mainboard неможливо отримати без ring-0, тому архітектура гібридна: сторонній, уже підписаний/встановлений моніторинговий застосунок робить низькорівневий доступ, а наш застосунок читає його опубліковані дані через named shared memory, WMI або HTTP.

> **Статус даних.** Усі назви об'єктів, структури й зміщення нижче звірені з vendor headers, SDK-документацією та незалежними open-source реалізаціями станом на листопад 2026. Версії HWiNFO 8.50, AIDA64 8.40, LibreHardwareMonitor 0.9.6, MSI Afterburner 4.6.x / RTSS 7.3.x. Оцінки продуктивності позначені **[оцінка]** — вони не є результатом власного бенчмарку й потребують підтвердження на цільовому залізі.

## Ключові висновки

- **HWiNFO64 Shared Memory — найкращий основний провайдер:** структурований бінарний формат, низький overhead, non-admin reader, добра підтримка заліза. Обмеження: HWiNFO треба встановити, free-версія вимикає SHM через 12 годин, bundle заборонено.
- **WMI у LibreHardwareMonitor більше немає.** Починаючи з 0.9.5 (січень 2026) провайдер видалено; актуальний шлях — HTTP `/data.json` або `/metrics`. WMI залишається лише для LHM ≤ 0.9.4 та OpenHardwareMonitor.
- **AIDA64 — платна альтернатива** з простим псевдо-XML у shared memory, але без версії, магії й timestamp; для більшості користувачів HWiNFO або LHM кращі.
- **Нічого не можна bundle-ити** з HWiNFO/AIDA64. Можна лише виявляти встановлений провайдер і вести користувача до офіційного завантаження.
- **Наш застосунок ніколи не підвищує права.** Він читає недовірені дані, валідує магію/версію/розміри/timestamp і деградує до «—», якщо провайдера немає.

---

## 1. HWiNFO64 Shared Memory

### 1.1. Kernel objects і доступ

| Елемент | Значення |
|---|---|
| File mapping | `Global\HWiNFO_SENS_SM2` |
| Mutex | `Global\HWiNFO_SM2_MUTEX` |
| Відкриття | `OpenFileMappingW(FILE_MAP_READ, FALSE, L"Global\\HWiNFO_SENS_SM2")` → `MapViewOfFile(..., FILE_MAP_READ, 0, 0, 0)` |
| Права читача | Адмін **не потрібен**; read-only відкриття працює зі звичайного user-процесу |
| Namespace | `Global\` — спільний для всіх Terminal Services сесій |

HWiNFO живе в `Global\` namespace, тому дані доступні навіть при fast user switching / RDP. Створення об'єкта в `Global\` вимагає `SeCreateGlobalPrivilege` (є у адмінів і сервісів), але **відкриття вже створеного — ні**; це прямо описано в документації Win32 про kernel object namespaces. Саме тому reader може бути неадміністраторським.

Окремо: у Pro-версії з Remote Sensor Monitoring кожне віддалене з'єднання публікується як `Global\HWiNFO_SENS_SM2_REMOTE_<index>` (індекс з нуля). Це той самий формат; для локального таскбара він не потрібен, але пояснює, чому в деяких системах є кілька схожих mapping-ів.

### 1.2. Заголовок `HWiNFO_SENSORS_SHARED_MEM2`

Офіційний header (`hwisenssm2.h`, постачається у HWiNFO SDK) оголошує `#pragma pack(1)`. Розмір — **48 байт (0x30)**.

| Offset | Поле | Тип | Примітка |
|---:|---|---|---|
| `0x00` | `dwSignature` | `DWORD` | байти `"HWiS"` = `48 57 69 53`; LE-константа `0x53695748`; `'DEAD'` коли неактивно |
| `0x04` | `dwVersion` | `DWORD` | `1` — початковий layout; `2` — додано UTF-8 поля (HWiNFO ≥ 7.33) |
| `0x08` | `dwRevision` | `DWORD` | `0` — до 6.11; `1` — додано `dwPollingPeriod` (6.11-3917+) |
| `0x0C` | `poll_time` | `__time64_t` (int64) | Unix-час останнього опитування, **у секундах** |
| `0x14` | `dwOffsetOfSensorSection` | `DWORD` | від початку mapping |
| `0x18` | `dwSizeOfSensorElement` | `DWORD` | stride для масиву sensors |
| `0x1C` | `dwNumSensorElements` | `DWORD` | кількість sensors |
| `0x20` | `dwOffsetOfReadingSection` | `DWORD` | від початку mapping |
| `0x24` | `dwSizeOfReadingElement` | `DWORD` | stride для масиву readings |
| `0x28` | `dwNumReadingElements` | `DWORD` | кількість readings |
| `0x2C` | `dwPollingPeriod` | `DWORD` | поточний polling period HWiNFO (з revision 1); одиниця в header не задокументована, на практиці читається як мілісекунди |

**Критично:** не можна хардкодити stride. HWiNFO сам повідомляє `dwSizeOfSensorElement` / `dwSizeOfReadingElement`, і вони змінювалися між ревізіями. У HWiNFO 8.50 live-перевірка дає `dwVersion=2`, `dwRevision=1`, sensor element **392 (0x188)**, reading element **460 (0x1CC)**.

Для `dwRevision == 0` (HWiNFO ≤ 6.11) поле `dwPollingPeriod` ще не існує, і логічний розмір заголовка — 44 байти. Читати 48 байт зазвичай безпечно (mapping вирівняний на сторінку), але покладатися на вміст `0x2C` не можна: перевіряйте `dwRevision >= 1`.

### 1.3. `HWiNFO_SENSORS_SENSOR_ELEMENT`

v1 — **264 байти (0x108)**:

| Offset | Поле | Тип |
|---:|---|---|
| `0x00` | `dwSensorID` | `DWORD` |
| `0x04` | `dwSensorInst` | `DWORD` |
| `0x08` | `szSensorNameOrig[128]` | `char` (ANSI) |
| `0x88` | `szSensorNameUser[128]` | `char` (ANSI) |

v2 додає в кінець `BYTE utfSensorNameUser[128]` за `0x108` → 392 байти.

Пара `(dwSensorID, dwSensorInst)` утворює стабільний ідентифікатор сенсора (наприклад, кілька однакових контролерів на одній платі).

### 1.4. `HWiNFO_SENSORS_READING_ELEMENT`

v1 — **316 байтів (0x13C)**:

| Offset | Поле | Тип | Примітка |
|---:|---|---|---|
| `0x00` | `tReading` | enum (4 байти) | тип величини |
| `0x04` | `dwSensorIndex` | `DWORD` | **індекс** у масиві sensors, не `dwSensorID` |
| `0x08` | `dwReadingID` | `DWORD` | унікальний у межах сенсора |
| `0x0C` | `szLabelOrig[128]` | `char` | оригінальна англійська назва |
| `0x8C` | `szLabelUser[128]` | `char` | назва, яку користувач міг перейменувати |
| `0x10C` | `szUnit[16]` | `char` | `"°C"`, `"RPM"`, `"V"`, `"W"`, `"MHz"`, `"%"` |
| `0x11C` | `Value` | `double` | поточне значення |
| `0x124` | `ValueMin` | `double` | мінімум за сесію |
| `0x12C` | `ValueMax` | `double` | максимум за сесію |
| `0x134` | `ValueAvg` | `double` | середнє |

v2 додає `BYTE utfLabelUser[128]` за `0x13C` та `BYTE utfUnit[16]` за `0x1BC` → 460 байтів.

`SENSOR_READING_TYPE`: `0 NONE`, `1 TEMP`, `2 VOLT`, `3 FAN` (RPM), `4 CURRENT`, `5 POWER` (W), `6 CLOCK` (MHz), `7 USAGE` (%), `8 OTHER`.

**Пастка для Rust:** `Value` лежить за `0x11C`, тобто **не вирівняний на 8 байт**. Struct є packed, тому не можна робити `&f64` на це поле — це UB і потенційний fault на ARM64. Читати через `ptr::read_unaligned` або парсити з `&[u8]` побайтово. `enum` у MSVC ABI — 4 байти, але покладатися на `#[repr(C)]` для всієї структури все одно не варто: краще явні offset-константи.

### 1.5. Семантика оновлення

- HWiNFO публікує дані зі швидкістю свого **polling period**, а не зі швидкістю вашого читання. За замовчуванням це 1–2 с; `dwPollingPeriod` повідомляє фактичне значення.
- `poll_time` — **цілі секунди**. Якщо читати частіше за 1 Гц, можна отримувати той самий `poll_time`; це нормальний спосіб відрізнити нові дані від повтору.
- Під час перепублікації HWiNFO **скидає `dwNumReadingElements` до нуля** і знову нарощує його, заповнюючи масив. Тому «нуль readings» — це не помилка, а стан гонки.
- HWiNFO **перестворює** mapping, якщо кількість сенсорів змінюється під час роботи (додали custom sensor). Якщо тримати mapping відкритим вічно, HWiNFO не зможе його перестворити; відомий баг із зависанням. Правильно: періодично перевідкривати mapping (наприклад, при зміні `dwNumSensorElements` або раз на кілька хвилин) і не тримати його роками.
- Mutex `Global\HWiNFO_SM2_MUTEX` — **advisory**. HWiNFO часто створює його з DACL, недоступним для non-admin, тому reader може не отримати handle. Практична стратегія: спробувати mutex з коротким таймаутом (10–50 мс), а якщо не вдалося — читати без нього, але робити **copy-and-recheck**: скопіювати блок, перечитати заголовок і відкинути копію, якщо `poll_time` або counts змінилися; повторити 2–3 рази.
- `ValueMin/Max/Avg` — це статистика HWiNFO за його власне вікно, не наші дані. Для таскбара достатньо `Value`.

### 1.6. 32/64-bit

Формат **не залежить від розрядності**: `DWORD` = 4 байти, `double` = 8, `__time64_t` = 8, покажчиків немає. 32-бітний Rust-застосунок може читати 64-бітний HWiNFO і навпаки. Єдині вимоги — правильні stride, packed-читання й урахування того, що `time_t` тут саме 64-бітний (не плутати з `time_t` у MAHM, де він 32-бітний).

### 1.7. Editions, ліміти, налаштування

| Питання | Відповідь |
|---|---|
| Де вмикається | HWiNFO → Settings → **Shared Memory Support**. У Sensors-only режимі — правий клік по tray-іконці → Settings |
| Default | **Вимкнено** з версії 7.00 (щоб не дратувати більшість користувачів) |
| Free edition | SHM працює **12 годин з моменту активації**, потім автоматично вимикається; потрібно **вручну** увімкнути знову. Це не «12 годин на добу», а 12 годин безперервної роботи з моменту старту таймера |
| Pro edition | Без ліміту. Окремого «Business» видання немає: є ліцензії Personal (non-commercial, 1 user / 5 PCs), Engineer (commercial, user-based) і Corporate (commercial, per-PC); усі вони активують Pro-режим і знімають 12-годинний ліміт |
| Commercial use | HWiNFO64 free — лише non-commercial; для комерційного використання потрібен Pro (Engineer/Corporate) |
| Elevation | HWiNFO **завжди** потребує admin (з 5.80, через kernel driver; manifest має `uiAccess=true`). Reader — ні |
| Auto-start | HWiNFO створює Task Scheduler task `HWiNFO` з найвищими правами; для non-admin користувача це вимагає збережених admin-credentials |

Важливо: безкоштовна версія після 12 годин не просто перестає оновлювати дані — **mapping зникає** (або signature стає невалідною). Наш застосунок має трактувати це як штатний стан «провайдер недоступний», а не як помилку.

### 1.8. Overhead

- Процес HWiNFO: ~30–80 МБ RSS, 0.5–2 % CPU при polling 1–2 с **[оцінка]**; залежить від кількості сенсорів і увімкнених графіків.
- Mapping: типово 50–400 КБ на великій платі (сотні readings × 316–460 байт) **[оцінка]**.
- Наше читання: OpenFileMapping+MapViewOfFile ~0.1–1 мс; повний прохід по масиву — десятки мікросекунд. При 1 Гц це **<0.05 % CPU** і <1 МБ RAM **[оцінка]**.

### 1.9. Rust-реалізації та крейти

| Крейт/проєкт | Що це | Ліцензія | Стан |
|---|---|---|---|
| `sensorwatch` 0.1.0 | Safe Rust bindings поверх native C core, читає саме `Global\HWiNFO_SENS_SM2` | MIT | Дуже новий (2026-06), десятки завантажень; API нестабільний, фактично один мейнтейнер |
| `sensorwatch-sys` 0.1.0 | Raw FFI до того ж C core; містить константи offset-ів і sanity-caps | MIT | Початковий |
| `rs-hwinfo2ha` | Rust-сервіс, що читає HWiNFO й публікує в MQTT/Home Assistant | MIT | Застосунок, не бібліотека |
| `hwinfo` 0.0.1 | SMBIOS, **не** shared memory | — | Не підходить |
| `hwinfo-rs` 0.1.0 | Bindings до `lfreist/hwinfo` (inventory: CPU/RAM/GPU), **не** сенсори | MIT | Не підходить |
| `winmmf` 0.6.2 | Safe wrapper над named shared memory на Windows | MIT/Apache | Корисна цеглина, але HWiNFO-парсер треба писати самому |
| `windows` crate | `OpenFileMappingW`, `MapViewOfFile` | MIT/Apache | Базовий варіант |

Висновок: **готового mature-крейта немає**. Власний парсер — це ~300–500 рядків разом із bounds-checking і тестами, що цілком прийнятно. Ризик `sensorwatch` як залежності — нульова репутація й одна людина-мейнтейнер; краще взяти його константи як reference, а не як dependency.

### 1.10. Ліцензування HWiNFO

- EULA забороняє **reverse engineering, декомпіляцію, модифікацію** і **bundling/embedding** HWiNFO у сторонні продукти без письмового дозволу REALiX.
- З версії **7.00** REALiX явно дозволив стороннім застосункам читати SHM, включно з комерційними: *«Yes, according to new conditions you're allowed to do so. HWiNFO32/64 version 7.0 or later shall be used on the client side»*. Це скасувало стару (2011) вимогу тримати інтерфейс закритим і робити застосунок безумовно безкоштовним.
- Отже: **читати SHM відкритим кодом — можна**, якщо користувач сам встановив HWiNFO 7.0+ і має відповідну ліцензію. Не можна: розповсюджувати HWiNFO, вбудовувати його, автоматизувати обхід 12-годинного ліміту (EULA прямо це забороняє), використовувати HWiNFO комерційно без Pro.
- Реалізація парсера за публічною специфікацією — це не reverse engineering. Але варто не копіювати пропрієтарні SDK-файли в репозиторій; достатньо власних констант і посилання на документацію.

---

## 2. AIDA64 Shared Memory

### 2.1. Формат

| Елемент | Значення |
|---|---|
| Object name | `AIDA64_SensorValues` |
| Namespace | Доступний і в **local**, і в **global** просторі; відкривати спочатку локальний, потім `Global\AIDA64_SensorValues` |
| Вміст | Один довгий **ANSI-рядок**, завершений `0x00`, змінної довжини |
| Формат | Псевдо-XML: теги є, але це **не** well-formed XML (немає єдиного root) |
| Мінімальний буфер читача | ≥ 10 КБ за документацією; реально 1–3 КБ, на старих Abit MicroGuru — більше |
| Кодування | ANSI/ASCII; у 7.99.7806 тимчасово був UTF-16LE, у 7.99.7807 повернули ANSI |
| Одиниці | Температури **завжди °C**, навіть якщо в GUI увімкнено Fahrenheit |
| Мови | Підписи **завжди англійською**, не локалізуються |

Приклад структури (спрощено):

```
<sys><id>OS</id><label>OS</label><value>Windows 11</value></sys>
<temp><id>TCPU</id><label>CPU</label><value>47.0</value></temp>
<fan><id>FCPU</id><label>CPU</label><value>1200</value></fan>
<volt><id>VCPU</id><label>CPU Core</label><value>1.250</value></volt>
<pwr><id>PCPU</id><label>CPU Package</label><value>65.2</value></pwr>
```

Групи, які реально зустрічаються: `sys`, `temp`, `fan`, `duty`, `volt`, `pwr` (подекуди `curr`). Кожен елемент має `<id>`, `<label>`, `<value>`; значення — рядок, парсити як `f64`.

AIDA64 також уміє публікувати **той самий псевдо-XML рядок** іншими каналами (усе вмикається в Preferences → External Applications):

| Канал | Назва / шлях | Примітка |
|---|---|---|
| Shared memory | `AIDA64_SensorValues` (local + global) | Основний, найшвидший |
| Registry | `HKCU\Software\FinalWire\AIDA64\SensorValues` | AIDA64 видаляє ключ при закритті |
| WMI | `Root\WMI\AIDA64_SensorValues` | Клас WMI; зручно для скриптів, але повільно |
| RTSS | `RTSSSharedMemoryV2` | Лише температури, напруги, вентилятори |

Для нас цікавий лише перший; решта — або повільні, або залежать від RTSS.

### 2.2. Особливості та стабільність

- **Немає заголовка, магії, версії, лічильників і timestamp.** Це головна слабкість: неможливо відрізнити «свіжі дані» від «застосунок завис, а рядок залишився». Максимум — евристика: якщо значення не змінювалося N секунд, позначити як stale.
- Довжина рядка змінюється під час роботи, тому читати треба до першого `0x00`, а не фіксований розмір.
- Перевірка кодування: якщо другий байт буфера — `0x00`, це UTF-16LE; інакше ANSI. Це рятує від регресій на кшталт 7.99.7806.
- Оскільки XML невалідний, звичайний XML-парсер впаде на «multiple root elements». Треба або обгорнути рядок у штучний `<root>...</root>`, або писати простий tag-scanner.

### 2.3. Editions, elevation, overhead

- External Applications доступні в **AIDA64 Extreme, Engineer, Business** (і, за мануалом, у відповідних продуктах лінійки). Network Audit має інший набір функцій.
- AIDA64 **потребує admin** для сенсорів: драйвер завантажується лише з-під адміністратора, а без нього зникають CPU/GPU/VRM температури й вентилятори. Reader — не потребує.
- Ціна (2026): Extreme — ~$65.95 / €59.95, **home use only**, 3 ПК; Engineer/Business — ~€219.90. Це суттєво дорожче за HWiNFO Pro.
- Overhead: процес AIDA64 ~60–150 МБ RSS, 1–3 % CPU **[оцінка]**; парсинг 1–3 КБ рядка — <100 µs.

### 2.4. Ліцензування AIDA64

Пропрієтарний продукт FinalWire. EULA забороняє redistribution, reverse engineering і передачу ключа. Читання опублікованого SHM для власного застосунку не заборонене (це задокументований інтерфейс), але **bundling AIDA64 неможливий**, а користувач має придбати ліцензію сам. Extreme не можна використовувати комерційно — для комерційного продукту потрібен Engineer/Business.

---

## 3. LibreHardwareMonitor (LHM)

### 3.1. WMI: стан на 2026 рік (важливо)

Історично LHM публікував дані в `root\LibreHardwareMonitor`:

| Клас | Поля |
|---|---|
| `Sensor` | `Identifier`, `Index`, `InstanceId` (key), `Max`, `Min`, `Name`, `Parent`, `ProcessId` (key), `SensorType`, `Value` |
| `Hardware` | `Name`, `Identifier`, `HardwareType`, `Parent` |

Запит: `Get-CimInstance -Namespace root/LibreHardwareMonitor -ClassName Sensor`, або WQL `SELECT Identifier, Name, Parent, Value FROM Sensor WHERE SensorType='Temperature'`.

Окремо вмикати WMI не потрібно: у LHM ≤ 0.9.4 провайдер реєструвався автоматично при старті застосунку (потрібен admin, бо реєстрація namespace — це адмін-операція). Саме тому WMI-дані з'являлися лише коли LHM запущений.

**Але у LHM 0.9.5 (січень 2026) WMI-провайдер видалено.** Причина, за словами мейнтейнера: .NET 10 більше не підтримує відповідний WMI provider API (`System.Management`). Namespace `root\LibreHardwareMonitor`, який бачать користувачі 0.9.5, — це **застаріла реєстрація від 0.9.4**, вона нічого не повертає. Офіційна рекомендація: інтегрувати `LibreHardwareMonitorLib` напряму або використовувати HTTP endpoint.

Наслідки для нас:

- WMI-шлях LHM актуальний лише для **LHM ≤ 0.9.4** і для **OpenHardwareMonitor**.
- Для нових LHM — тільки HTTP або in-process.
- WMI як транспорт усе ще має сенс як legacy-fallback, але це не пріоритет.

WMI також потребує, щоб LHM був запущений (namespace реєструється під час старту), і LHM майже завжди запускається elevated. Сам запит із non-admin процесу до локального namespace зазвичай працює, але latency висока: 10–100 мс на перший запит, 1–10 мс на повторні **[оцінка]**; WmiPrvSE додає помітний CPU при частих запитах.

### 3.2. HTTP server

LHM має вбудований `HttpListener`:

| Параметр | Значення |
|---|---|
| Default port | `8085` |
| Default listen IP | **`0.0.0.0`** (усі інтерфейси). У формі вибору IP останній пункт — `0.0.0.0`, і саме він вибирається за замовчуванням |
| Auth | Опційна HTTP **Basic**; пароль зберігається як SHA-256, передається **відкритим текстом** |
| Endpoints | `GET /` (UI), `GET /data.json`, `GET /metrics`, `GET` або `POST /Sensor`, `GET /ResetAllMinMax` |
| Gzip | Підтримується, якщо клієнт надіслав `Accept-Encoding: gzip` |
| CORS | `Access-Control-Allow-Origin: *` |
| Запуск | Options → Remote Web Server → **Run**; можна зберегти в налаштуваннях, тоді сервер піднімається при старті |

> `/data.html` у LHM **немає** — це поширена плутанина. Людський UI віддається на `/` (`index.html`), а машинні дані — на `/data.json` і `/metrics`.

`/data.json` — ієрархічне дерево вузлів:

```json
{
  "Text": "AMD Ryzen 7 7800X3D",
  "SensorId": "/amdcpu/0",
  "Type": "Cpu",
  "Value": "",
  "Min": "",
  "Max": "",
  "ImageURL": "images_icon/cpu.png",
  "Children": [
    { "Text": "Core (Tctl/Tdie)", "SensorId": "/amdcpu/0/temperature/0",
      "Type": "Temperature", "Value": "51.5 °C", "RawValue": "51500",
      "Min": "40.0 °C", "Max": "72.0 °C", "Children": [] }
  ]
}
```

Практичні пастки:

- `Value` — **форматований рядок**, а не число; локаль може давати кому замість крапки (`"51,5 °C"`). Треба брати числовий префікс і нормалізувати кому.
- `SensorId` **не гарантовано унікальний** (зустрічається дубль `/gpu-nvidia/0/load/3`).
- `Value`/`RawValue` історично змінювали тип між релізами (рядок/число/null) — парсер має бути толерантним.

`/metrics` — Prometheus/OpenMetrics, зручніший для машинного читання:

```
lhm_cpu_temperature_celsius{sensorName="Core (Tctl/Tdie)",sensorId="/temperature/0",hardwareId="/amdcpu/0",host="..."} 51.5
```

Метрики мають `InvariantCulture` (крапка), базові одиниці (Hz, W, °C, rpm) і стабільніші назви. Для таскбара це кращий вибір, ніж `data.json`: менший payload і простіший парсинг.

Продуктивність HTTP **[оцінка]**: `data.json` на великій системі — 100–800 КБ без стиснення, 10–30 КБ gzip; серіалізація на боці LHM — 1–10 мс; наш HTTP-запит + JSON-парсинг — 2–15 мс; при 1 Гц це 0.1–0.5 % CPU. `metrics` зазвичай у 2–5 разів менший.

### 3.3. Elevation і драйвери

- LHM маніфест вимагає **administrator**. Без цього частина сенсорів (CPU package, VRM, fan) просто відсутня.
- З версії **0.9.5** LHM перейшов з **WinRing0** на **PawnIO** (PR #1857). WinRing0 визначається Microsoft Defender як `HackTool:Win32/Winring0` і блокується; PawnIO — підписаний драйвер із модулями на мові Pawn, які підписує мейнтейнер, і він дає доступ лише до тих операцій, для яких написаний модуль.
- PawnIO встановлюється окремо (winget `namazso.PawnIO`) і вимагає admin/UAC. LHM має опцію не встановлювати низькорівневий driver, але тоді частина сенсорів зникне.
- Для нас це означає: якщо ми залежимо від LHM, ми залежимо від **наявності підписаного kernel driver** у системі користувача. Це нормально для геймерського ПК, але неприйнятно як вимога для корпоративного ноутбука.

### 3.4. In-process LibreHardwareMonitorLib з Rust

Технічно можливо:

1. `LibreHardwareMonitorLib` — .NET-бібліотека (NuGet), цілі `net10.0` і `net472`.
2. Rust вантажить .NET runtime через `nethost` + `hostfxr` (`hostfxr_initialize_for_runtime_config`, `hostfxr_get_runtime_delegate`), або через крейт **`netcorehost` 0.22.0** (MIT), який дає `load_hostfxr()`, `initialize_for_runtime_config()`, `AssemblyDelegateLoader`.
3. Далі — delegate на C#-метод, який повертає масив значень через `IntPtr`/`sizeBytes`.

Чому це погана ідея для таскбара:

- **Framework-dependent only.** Офіційні hosting API не підтримують self-contained deployment. Потрібен встановлений .NET runtime (10.0 або сумісний) — або ми тягнемо його з собою, що роздуває інсталятор на 60–100 МБ.
- **Один runtime на процес.** Якщо в нас колись з'явиться інша .NET-залежність з іншою версією — конфлікт; `hostfxr` поверне failure.
- **Немає crash isolation.** NullReferenceException або AV у нативній частині LHM = падіння **всього** таскбар-застосунку. Для UI, який живе в explorer.exe-сусідстві, це неприйнятно.
- **GC-паузи й пам'ять.** CoreCLR + LHM + WinForms-залежності — це 80–200 МБ RSS і кілька потоків; для таскбара це надлишково.
- **Права.** LHM lib для CPU-температур потребує admin. Ми або запускаємо весь застосунок elevated (погано: таскбар не повинен бути адміном), або робимо окремий elevated helper — що повертає нас до архітектури «окремий процес», але вже з нашим власним кодом і нашим драйвером.
- **MPL 2.0.** Лінкування дозволене, але модифікації LHM-коду треба публікувати. Це не блокер, але додає юридичної роботи.
- **C++/CLI не рятує.** Проміжний C++/CLI-шар усе одно завантажує CoreCLR у наш процес, тож усі проблеми з падінням, GC і admin-правами залишаються; додається лише складність збірки й ABI.

Висновок: in-process LHM — **не рекомендовано**. Якщо вже хастити .NET, дешевше запускати LHM як **зовнішній процес** і читати HTTP.

---

## 4. Інші провайдери

### 4.1. OpenHardwareMonitor (OHM) WMI — legacy

- Namespace `root\OpenHardwareMonitor`, класи `Hardware` і `Sensor`; базові поля ті самі, що в LHM (`Identifier`, `Name`, `SensorType`, `Parent`, `Value`, `Min`, `Max`, `Index`), але без `InstanceId`/`ProcessId`, які LHM додав пізніше.
- `SensorType`: `Voltage`, `Clock`, `Temperature`, `Load`, `Fan`, `Flow`, `Control`, `Level`.
- Останній реліз — **0.9.6 (грудень 2020)**. Проєкт фактично закинутий; нові CPU/плати не підтримуються. Є форк `HardwareMonitor/openhardwaremonitor`, але це окремий проєкт.
- WMI все ще працює, бо OHM — .NET Framework 4.x і не залежить від .NET 10.
- Використовувати лише як останній fallback для старих систем.

### 4.2. MSI Afterburner / RivaTuner (MAHM, RTSS)

**MAHM** (MSI Afterburner Hardware Monitoring):

| Елемент | Значення |
|---|---|
| Mapping | `MAHMSharedMemory` |
| Signature | `'MAHM'` = `0x4D41484D`; `0xDEAD` — деалокація |
| Version | `0x00020000` для v2.0 |
| Header | `dwSignature`, `dwVersion`, `dwHeaderSize`, `dwNumEntries`, `dwEntrySize`, `time` (32-bit `time_t`), `dwNumGpuEntries`, `dwGpuEntrySize` — 32 байти |
| Entry size | 1324 байти: 5 × `char[260]` (`szSrcName`, `szSrcUnits`, `szLocalizedSrcName`, `szLocalizedSrcUnits`, `szRecommendedFormat`), `float data`, `float minLimit`, `float maxLimit`, `DWORD dwFlags`, `DWORD dwGpu`, `DWORD dwSrcId` |

`dwSrcId` — ключове поле: `0x00000000` GPU temperature, `0x00000080` **CPU temperature**, `0x00000090` CPU usage, `0x00000010` fan speed, `0x00000020` core clock, `0x00000050` framerate. `dwGpu == 0xFFFFFFFF` означає глобальний показник. `data == FLT_MAX` — sentinel «недоступно». `time` — UTC Unix time, 32-бітний (Y2038).

Переваги: Afterburner часто вже стоїть на геймерських ПК, має CPU-температуру (якщо ввімкнено в моніторингу), нулевий overhead читання. Недоліки: дані залежать від того, які сенсори користувач увімкнув у GUI; порядок entries нестабільний — шукати за назвою/`dwSrcId`, а не за індексом; closed-source freeware, розповсюджувати не можна.

**RTSS** (`RTSSSharedMemoryV2`): signature `'RTSS'`, version `0x0002xxxx`, заголовок із `dwAppEntrySize`, `dwAppArrOffset`, `dwAppArrSize`, `dwOSDEntrySize`, `dwOSDArrOffset`, `dwOSDArrSize`, `dwOSDFrame`, `dwBusy`. Application entry: `dwProcessID`, `szName[260]`, `dwFlags`, `dwTime0`, `dwTime1`, `dwFrames`, `dwFrameTime`, плюс статистика min/avg/max і frametime-буфер. **Температур там немає** — тільки FPS/frametime. Для таскбара може бути окремим джерелом FPS, не більше.

**Anti-cheat:** саме RTSS/Afterburner роблять ін'єкцію overlay у процес гри, і це може конфліктувати з EAC/BattlEye/Vanguard. **Наше читання shared memory не інжектить нічого** і не має цієї проблеми. Але якщо ми показуємо FPS, варто попередити користувача, що RTSS може бути заблокований античитом.

### 4.3. CapFrameX

MIT-ліцензія, але це інструмент захоплення frametime на базі PresentMon, а не сервіс сенсорів. Він використовує власний форк LHM, має вбудований MCP-сервер на `localhost:1337/mcp` (read-only), але не публікує загальнодоступний API температур. Як залежність — не має сенсу.

### 4.4. HWMonitor (CPUID)

Free і PRO версії, PRO — €19.95/€34.95, remote monitoring через власний TCP-порт **25021**, протокол закритий. Публічного API/shared memory немає. **Не підходить.**

### 4.5. Core Temp — безкоштовний CPU-only fallback

| Елемент | Значення |
|---|---|
| Mapping | `CoreTempMappingObjectEx` (v2), застарілий `CoreTempMappingObject` |
| Namespace | **Session-local**: `\Sessions\N\BaseNamedObjects\CoreTempMappingObjectEx` |
| Struct | `CoreTempSharedDataEx`: `uiLoad[256]`, `uiTjMax[128]`, `uiCoreCnt`, `uiCPUCnt`, `fTemp[256]`, `fVID`, `fCPUSpeed`, `fFSBSpeed`, `fMultiplier`, `sCPUName[100]`, `ucFahrenheit`, `ucDeltaToTjMax`, `ucTdpSupported`, `ucPowerSupported`, `uiStructVersion`, `uiTdp[128]`, `fPower[128]`, `fMultipliers[256]` |
| Alignment | 4 байти (`#pragma pack(4)`) |
| Версія | `uiStructVersion = 2` |

Дає лише CPU (per-core temp/load, TjMax, іноді power). Безкоштовний, маленький, не потребує admin для читання (але сам Core Temp потребує driver/admin для сенсорів). Через session-local namespace не працює з іншої сесії — для таскбара в тій самій сесії це ок.

### 4.6. LHM через PowerShell

`Get-CimInstance -Namespace root/LibreHardwareMonitor -ClassName Sensor` — зручно для діагностики, але кожен виклик — це запуск PowerShell (~300–500 мс) плюс WMI. Для runtime **не використовувати**; максимум — кнопка «Diagnose» в налаштуваннях.

---

## 5. Порівняння провайдерів

| Провайдер | Транспорт | Свіжість / latency | CPU нашого процесу (est.) | RAM нашого процесу (est.) | Провайдер потребує admin | Наш застосунок потребує admin | Ліцензія | Придатність |
|---|---|---|---|---|---|---|---|---|
| **HWiNFO 7.0+** | Shared memory `Global\HWiNFO_SENS_SM2` | дані 1–2 с; читання <0.5 мс | <0.05 % @1 Гц | <1 МБ + 50–400 КБ mapping | Так | **Ні** | Free 12 h; Pro unlimited; bundling заборонено | ★★★★★ основний |
| **AIDA64** | Shared memory `AIDA64_SensorValues` | дані ~1 с; парсинг <0.1 мс | <0.05 % @1 Гц | <1 МБ + 1–3 КБ | Так | **Ні** | Proprietary; Extreme home-only; bundling заборонено | ★★★★☆ |
| **LHM 0.9.5+** | HTTP `127.0.0.1:8085/metrics` | дані 1 с; запит 2–15 мс | 0.1–0.5 % @1 Гц | 1–2 МБ буфери | Так | **Ні** | MPL 2.0 (можна bundle незмінені binaries) | ★★★★☆ основний безкоштовний |
| **LHM ≤0.9.4 / OHM** | WMI `root\...\Sensor` | дані 1 с; запит 10–100 мс | 0.2–1 % @1 Гц | 1–3 МБ | Так | Ні | MPL 2.0 | ★★☆☆☆ legacy |
| **MSI Afterburner** | Shared memory `MAHMSharedMemory` | дані 0.5–1 с; <0.5 мс | <0.05 % @1 Гц | <1 МБ | Так (для своїх сенсорів) | **Ні** | Freeware, closed; redistribution заборонено | ★★★☆☆ gaming fallback |
| **Core Temp** | Shared memory `CoreTempMappingObjectEx` | ~1 с; <0.2 мс | <0.02 % @1 Гц | <1 МБ | Так | **Ні** | Freeware | ★★☆☆☆ CPU-only |
| **RTSS** | Shared memory `RTSSSharedMemoryV2` | 1 с; <0.5 мс | <0.02 % | <1 МБ | Ні для читання (сам RTSS зазвичай elevated) | Ні | Freeware, closed | ★☆☆☆☆ тільки FPS |
| **CapFrameX** | MCP HTTP :1337 | н/д | н/д | н/д | Ні | Ні | MIT | ✗ не сервіс сенсорів |
| **HWMonitor** | Немає публічного API | — | — | — | Так | — | Proprietary | ✗ |
| **LHM in-process** | FFI/hostfxr | <0.1 мс | 1–3 % + GC | 80–200 МБ | **Так** | **Так** | MPL 2.0 | ✗ крихкість |

**Пояснення до таблиці.** «CPU нашого процесу» — це витрати на читання й парсинг, без урахування процесу-провайдера. HWiNFO/AIDA64/Afterburner/Core Temp самі споживають 0.5–3 % CPU і 30–150 МБ RAM; LHM — 1–3 % і 100–300 МБ. Усі цифри — **[оцінка]** на основі типових конфігурацій; на реальному залізі вони залежать від кількості сенсорів, polling period і версії. Є поодинокі повідомлення про ~10 % CPU у LHM на специфічному ноутбучному залізі (ThinkPad P15), що добре ілюструє: «типові» цифри не є гарантією для кожного пристрою.

---

## 6. Архітектурна оцінка

### 6.1. Latency та overhead

Для таскбара з оновленням раз на 1–2 секунди **будь-який** із цих провайдерів дає достатню свіжість; різниця в latency (0.5 мс vs 15 мс) невидима оком. Реальна різниця — у **вартості володіння**:

- Shared memory — найдешевший транспорт: нуль копіювань, нуль серіалізації, вся робота — bounds-check і кількадесят читань.
- HTTP — найдорожчий серед «нормальних»: серіалізація всього дерева на боці LHM + TCP loopback + JSON-парсинг. Але це все одно мілісекунди, і при 1 Гц ним можна знехтувати.
- WMI — найдорожчий за latency й найгірший за передбачуваність: залежить від стану WmiPrvSE, може блокуватися на секунди, має історію витоків пам'яті при частих запитах.

### 6.2. Auto-start і життєвий цикл

- Наш застосунок має стартувати **в user session** (HKCU `Run` або Task Scheduler з `LeastPrivilege`), без UAC. Інакше таскбар не зможе взаємодіяти з explorer.
- Провайдери стартують самі: HWiNFO — Task Scheduler task `HWiNFO` (admin), AIDA64 — власний autostart (admin), LHM — «Run On Windows Startup» (admin task), Afterburner — звичайний autostart.
- Наш застосунок **не повинен** запускати провайдери за користувача: по-перше, це вимагає elevation; по-друге, HWiNFO EULA забороняє автоматизацію обходу лімітів; по-третє, це несподівано для користувача.
- Проба провайдерів — **асинхронно після першого paint**, з таймаутом 50–100 мс на провайдера. Ніколи не блокувати UI-потік.

### 6.3. Failure modes

| Ситуація | Симптом | Що робити |
|---|---|---|
| Провайдер не встановлений | `OpenFileMapping` → `ERROR_FILE_NOT_FOUND`; HTTP → connection refused; WMI → namespace missing | Показати «—» і посилання «Як увімкнути температуру» |
| SHM вимкнено в налаштуваннях | mapping відсутній | Те саме; підказати перевірити Settings → Shared Memory Support |
| HWiNFO free 12 h expiry | mapping зник під час роботи | Позначити stale, показати tooltip «HWiNFO Shared Memory вимкнено (12 h limit)» |
| HWiNFO перестворює mapping | раптовий нуль readings / зміна counts | Перевідкрити mapping, повторити з backoff |
| Version mismatch | невідомий `dwVersion`, замалий `dwSizeOf...Element` | Відмовитися від провайдера, залогувати версію; не намагатися «вгадати» layout |
| Stale data | `poll_time` не змінюється | Позначити як stale через 3–5 с; приховати через 10–15 с |
| LHM 0.9.5+ WMI | namespace є, даних немає | Не використовувати WMI для LHM ≥0.9.5; перейти на HTTP |
| HTTP порт 8085 зайнятий | LHM сервер не стартує | Показати помилку; дозволити інший порт у налаштуваннях |
| LHM слухає 0.0.0.0 | доступний з мережі | Попередити; рекомендувати `127.0.0.1` + firewall |
| Locale comma | `"51,5 °C"` не парситься як f64 | Нормалізувати кому/крапку, парсити префікс |
| Дублікати SensorId | два вузли з однаковим id | Ключувати за `(hardwareId, sensorId, index)` або за назвою |
| Провайдер оновився | змінилися назви сенсорів | Мати alias-таблицю й fallback-евристики |

### 6.4. Безпека

**Хто може читати shared memory?**

DACL file-mapping об'єкта береться з токена творця. Для секції, створеної від `SYSTEM`, default DACL дає `GENERIC_ALL` лише `SYSTEM` і `Administrators`, а звичайним користувачам — нічого. Для секції, створеної від імені адміністратора, у default DACL зазвичай присутній SID самого користувача, тому його ж non-elevated процес може отримати read. На практиці HWiNFO і AIDA64 створюють mapping із DACL, що дозволяє read для `Users`/`INTERACTIVE` — інакше жоден Rainmeter/Stream Deck плагін не працював би. Незалежні reader-и підтверджують: **non-admin читає HWiNFO без проблем**. Але покладатися на це не можна: якщо DACL раптом стане суворішим, ми отримаємо `ERROR_ACCESS_DENIED`, і це треба обробити як штатну ситуацію.

**Namespace і сесії.** `Global\` бачать усі сесії; `Local\` — лише поточна. HWiNFO/AIDA64 — `Global\`, тому працюють при RDP і fast user switching. Core Temp — session-local, тому з іншої сесії його не видно. Наш reader має відкривати саме `Global\`, інакше він «випадково» читатиме чужий об'єкт у своїй сесії.

**UIPI та integrity levels.** UIPI блокує window messages, а не shared memory. Mandatory Integrity Control застосовується до section objects лише за наявності mandatory label (SACL); типово його немає. Тому medium-IL процес може читати high-IL процес, якщо DACL дозволяє. Це і є основою всієї архітектури «admin-monitor → user-taskbar».

**Spoofing.** Named object — це глобальне ім'я; хто перший створив, той і володіє. Звичайний користувач **не може** створити object у `Global\` (потрібен `SeCreateGlobalPrivilege`), тому low-priv spoofing неможливий. Але:
- адмін-процес або malware з admin правами може створити підроблений `Global\HWiNFO_SENS_SM2` і годувати нас фейковими температурами;
- у `Local\` namespace будь-який процес користувача може створити object-двійник.

Тому: завжди відкривати `Global\`, перевіряти магію/версію/розміри, і ставитися до даних як до **недовірених**. Практична шкода від підробки — неправдиві показання, не RCE. Але якщо ми колись робитимемо дії на основі температур (наприклад, автоматичне керування вентиляторами), це стає security-питанням.

**Валідація (обов'язкова):**

1. `dwSignature == 0x53695748` (байти `HWiS`).
2. `dwVersion` ∈ {1, 2}; `dwRevision` ∈ {0, 1}; невідомі — відмова.
3. `dwSizeOfSensorElement >= 264` і `<= 4096`; `dwSizeOfReadingElement >= 316` і `<= 4096`.
4. `dwNumSensorElements <= 4096`; `dwNumReadingElements <= 65536`.
5. `dwOffsetOfSensorSection + size*count <= mapped_size`; те саме для readings; перевіряти overflow у `u64`.
6. Рядки — NUL-terminated у межах поля; для v2 — валідний UTF-8 (або lossy).
7. `Value` — скінченний `f64`; температура в межах, скажімо, −40…150 °C; відсікати `0`, `NaN`, `±inf`, `FLT_MAX`.
8. `poll_time` — не старіший за N секунд; монотонність не гарантована (годинник може стрибати).
9. Для AIDA64: перевіряти, що рядок складається з відомих тегів, довжина < 64 КБ, є `<temp>`; немає магії — компенсувати перевіркою діапазонів і свіжості.
10. Для MAHM: `dwSignature == 'MAHM'`, `dwVersion >= 0x00020000`, `dwHeaderSize == 32`, `dwEntrySize >= 1324`, `dwNumEntries <= 4096`, `time` не старіший за 15 с, `data != FLT_MAX`.

**HTTP-специфіка.** LHM за замовчуванням слухає `0.0.0.0:8085` **без автентифікації**. Це означає, що будь-хто в LAN може читати температури, а через `POST /Sensor` — **змінювати швидкість вентиляторів**. Для нашого застосунку: з'єднуватися лише на `127.0.0.1`, перевіряти, що `ListenerIp` у налаштуваннях LHM — loopback, і рекомендувати firewall-правило. Basic auth — лише по HTTP, тому пароль іде відкритим текстом; не використовувати його для віддаленого доступу без TLS-проксі.

**WMI-специфіка.** Локальний WMI-запит безпечний, але remote WMI (DCOM, порт 135 + dynamic RPC) — велика поверхня атаки. Нам remote не потрібен; не вмикати.

### 6.5. Ліцензії та розповсюдження

| Дія | HWiNFO | AIDA64 | LHM | OHM | Afterburner/RTSS | Core Temp |
|---|---|---|---|---|---|---|
| Читати опубліковані дані | Так (7.0+) | Так | Так | Так | Так | Так |
| Наш застосунок open-source | Так | Так | Так | Так | Так | Так |
| Bundling/redistribution провайдера | **Ні** | **Ні** | Так, незмінені binaries + MPL notice | Так, MPL | **Ні** | **Ні** |
| Автоматичне завантаження/встановлення | **Ні** (заборонено EULA) | **Ні** | Можливо, але драйвер PawnIO — окремо | — | **Ні** | **Ні** |
| Комерційне використання нашого застосунку | Так, але користувач має HWiNFO Pro | Потрібен Engineer/Business | Так (MPL) | Так | Так | Так |
| Обхід 12 h ліміту | **Заборонено** | — | — | — | — | — |

**Що казати користувачу:** «Для температур встановіть HWiNFO64 (безкоштовно для особистого використання) або LibreHardwareMonitor. Ми не постачаємо ці програми; посилання — на офіційний сайт». Не можна обіцяти «працює з коробки», якщо єдина гарантія — це те, що в користувача вже щось стоїть.

**Чи можна bundle LHM?** MPL 2.0 дозволяє розповсюджувати **незмінені** binaries разом із нашим застосунком, за умови надання ліцензії та доступу до source. Але LHM тягне PawnIO driver, який треба встановлювати з admin/UAC; це змінює модель загрози й ускладнює інсталятор. Тому краще **не bundle**, а залежати від user-installed LHM.

---

## 7. Рекомендація

### 7.1. Пріоритет провайдерів

1. **HWiNFO64 Shared Memory** — основний.
   Найкраще покриття (CPU package, Tctl/Tdie, VRM, motherboard, fan, power), структурований бінарний формат, мінімальний overhead, працює з non-admin reader. Мінуси: потребує встановленого HWiNFO, 12-годинний ліміт у free, закритий EULA на bundling. Для комерційного продукту — Pro у користувача.

2. **LibreHardwareMonitor HTTP `/metrics`** — основний безкоштовний/open-source.
   Найкраще покриття серед безкоштовних, активна підтримка, MPL 2.0. Мінуси: LHM 0.9.5+ більше не має WMI, тому тільки HTTP; LHM потребує admin; сервер за замовчуванням слухає всі інтерфейси; HTTP дорожчий за SHM. Використовувати `127.0.0.1:8085/metrics`, а не `data.json`, якщо потрібна швидкість/стабільність парсингу.

3. **AIDA64 Shared Memory** — альтернатива для тих, у кого вже є AIDA64.
   Простий формат, низький overhead, але платний, без версії/timestamp і з історією регресій кодування. Не робити основним.

4. **MSI Afterburner / MAHM** — gaming fallback.
   Безкоштовний, часто вже стоїть, має `dwSrcId=0x80` CPU temperature. Але залежить від конфігурації GUI, GPU-центричний, closed-source.

5. **Core Temp** — CPU-only fallback.
   Безкоштовний, маленький, але лише температури ядер; session-local namespace.

6. **WMI (OpenHardwareMonitor, LHM ≤0.9.4)** — legacy.
   Лише для старих систем; висока latency, нестабільна поведінка, LHM 0.9.5+ не підтримує.

**Не використовувати:** HWMonitor (немає API), CapFrameX (не сервіс сенсорів), in-process LHM (крихкість, admin, .NET), будь-який bundling HWiNFO/AIDA64.

### 7.2. Дизайн `SensorProvider` (interface sketch, без коду)

Трейт описує **джерело**, а не конкретний сенсор. Всі методи — `&self`/`&mut self`, крім `probe`, який може бути `async` або виконуватися в окремому потоці.

**Associated types / data:**

- `ProviderId` — стабільний enum: `Hwinfo`, `Lhm`, `Aida64`, `Mahm`, `CoreTemp`, `WmiOhm`, `WmiLhm`.
- `ProviderInfo { name, version: Option<Version>, vendor, requires_elevation: bool, capabilities: Capabilities }`.
- `Capabilities` — бітова маска: `TEMPERATURE`, `FAN`, `VOLTAGE`, `POWER`, `CLOCK`, `LOAD`, `FPS`.
- `SensorKey { provider, hardware_id: String, sensor_id: String, instance: u32 }` — стабільний ключ для зіставлення між сесіями.
- `SensorKind` — `CpuPackageTemp`, `CpuCoreTemp(u32)`, `MainboardTemp(u32)`, `FanRpm(u32)`, `Voltage(u32)`, `Power(u32)`, `Other`.
- `Reading { key, kind, label: String, value: f64, unit: Unit, quality: Quality }`, де `Quality` — `Good | Stale | Suspect | Unavailable`.
- `SensorSnapshot { provider, captured_at: Instant, provider_timestamp: Option<SystemTime>, readings: Vec<Reading>, diagnostics: Vec<Diagnostic> }`.
- `ProviderError` — `NotInstalled`, `NotRunning`, `Disabled`, `AccessDenied`, `VersionUnsupported`, `MalformedData`, `Timeout`, `Io`, `Internal`.

**Методи:**

| Метод | Призначення |
|---|---|
| `fn id(&self) -> ProviderId` | Ідентифікація |
| `fn probe(&self) -> Availability` | Дешева перевірка: файл є? process є? HTTP відповідає? WMI namespace є? Повертає `Ready`, `Degraded(reason)`, `Unavailable(reason)` |
| `fn info(&self) -> ProviderInfo` | Версія, vendor, capabilities, чи потрібен elevation провайдеру |
| `fn open(&mut self) -> Result<(), ProviderError>` | Відкрити mapping/HTTP-клієнт/WMI-сесію; ідемпотентно |
| `fn poll(&mut self, budget: Duration) -> Result<SensorSnapshot, ProviderError>` | Основна операція; **не** блокує довше за `budget`; повертає snapshot або помилку |
| `fn close(&mut self)` | Звільнити handle/mapping; викликати при виході й при зміні провайдера |
| `fn describe_sensors(&self) -> Vec<SensorDescriptor>` | Метадані без значень — для UI вибору сенсора й для alias-таблиці |

**Політика registry (не трейт, а шар над ним):**

- Упорядкований список інстансів; перший `Ready` — активний, решта — пасивні.
- Стан-машина: `Unknown → Probing → Ready → Degraded → Unavailable`; backoff 1 s → 2 s → 5 s → 30 s → 60 s.
- Кожен `poll` має дедлайн; повільний провайдер не блокує інших (окремий потік або `spawn_blocking`).
- Останній good snapshot зберігається; після 3–5 с без оновлення — `Quality::Stale`, після 10–15 с — `Unavailable`, UI показує «—».
- Нормалізація: `°C` завжди; значення `f64`; відсікання `NaN/inf` і діапазонів.
- Resolution rules для CPU temp: `CPU Package` → `Core (Tctl/Tdie)` → `CPU Die` → `Core Max` → максимум по ядрах; **ніколи** не брати `Distance to TjMax` як температуру (це дельта).
- Mainboard: `Motherboard`, `System`, `VRM`; якщо кілька — показати найгарячіший або дати вибір.
- Alias-таблиця для назв сенсорів, бо HWiNFO/AIDA64/LHM називають одне й те саме по-різному.

**Деградація:**

| Стан | UI |
|---|---|
| Жоден провайдер не Ready | Рядок температури прихований або «—»; tooltip «Немає джерела температур» + кнопка «Налаштувати» |
| Провайдер є, але 12 h expiry | «—» + tooltip «HWiNFO Shared Memory вимкнено» |
| Дані stale | Останнє значення сірим, суфікс «~», tooltip з віком |
| Помилка доступу | «—» + tooltip «Провайдер не дозволяє читання» (не пропонувати elevation нашого застосунку) |
| Часткові дані (тільки CPU) | Показати CPU, решту приховати |

**Налаштування користувача:** увімкнення/вимкнення кожного провайдера, ручний пріоритет, polling interval (1–5 с), кнопка «Перевірити зараз» із діагностикою (версія, останній `poll_time`, кількість сенсорів, причина помилки). Це різко зменшує кількість звернень у підтримку.

### 7.3. Що сказати в документації

- «Температури потребують стороннього моніторингового застосунку, який має kernel driver. Ми не встановлюємо драйверів і не працюємо від адміністратора».
- «Рекомендуємо HWiNFO64 (Sensors-only, Shared Memory Support) або LibreHardwareMonitor (Remote Web Server на 127.0.0.1:8085)».
- «HWiNFO free вимикає Shared Memory через 12 годин; для постійної роботи потрібен Pro».
- «Ми не постачаємо HWiNFO/AIDA64; завантажуйте з офіційних сайтів».
- «Якщо температура не показується — перевірте, що провайдер запущений і що в ньому ввімкнено відповідний інтерфейс».

---

## 8. Sources

**HWiNFO**
1. Офіційний header `hwisenssm2.h` (v6.11-3917) — https://github.com/MintyMods/MintySensorMonitor/blob/master/hwinfo/hwisenssm2.h_v6.11.3917
2. `nwinfo` HWiNFO SHM reader із актуальними v2 полями — https://github.com/a1ive/nwinfo/blob/master/libnw/sensor/hwinfo_shmem.c
3. Технічний довідник із live-перевіркою на HWiNFO 8.50 — https://github.com/dranben/karen-hud/blob/main/docs/hwinfo-shared-memory.md
4. Reverse-engineered layout (namazso) — https://gist.github.com/namazso/0c37be5a53863954c8c8279f66cfb1cc
5. HWiNFO licenses / 12-hour limit — https://www.hwinfo.com/licenses/
6. HWiNFO 7.00 зміни та FAQ про SHM — https://www.hwinfo.com/forum/threads/important-changes-to-hwinfo64-coming-soon.7092/
7. HWiNFO EULA (PDF) — https://www.hwinfo.com/files/license.pdf
8. Про bundling/заборону embedding — https://www.hwinfo.com/forum/threads/hwifo-block-our-project.7730/
9. Про admin-вимогу — https://www.hwinfo.com/forum/threads/persistent-driver-not-being-persistent-always-asks-for-admin-rights.5194/
10. Про перестворення mapping і mutex — https://www.hwinfo.com/forum/threads/hwinfo64-crash-with-shared-memory-and-custom-sensors.7868/
11. Про polling rate SHM — https://www.hwinfo.com/forum/threads/shared-memory-update-rate.10553/
12. Приклад .NET-reader із mutex/staleness — https://github.com/Seraksab/Hwinfo.SharedMemory.Net
13. HWiNFO SDK — https://www.hwinfo.com/sdk/

**AIDA64**
14. External Applications (shared memory, registry, WMI, RTSS) — https://www.aida64.com/user-manual/hardware-monitoring/external-applications
15. AIDA64 Engineer: Shared Memory — https://documentation.help/AIDA64/sharedmemory.htm
16. Регресія UTF-16 у 7.99.7806 і повернення ANSI — https://forums.aida64.com/topic/18385-fixed-aida64-v799-beta-aida64_sensorvalues-bug/
17. AIDA64 licensing — https://www.aida64.com/licensing
18. AIDA64 editions comparison — https://www.aida64.com/compare-aida64-features
19. AIDA64 Extreme store — https://www.aida64.com/online-store/aida64-extreme

**LibreHardwareMonitor**
20. Репозиторій і README (MPL 2.0, admin, приклад LibreHardwareMonitorLib) — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
21. WMI removal у 0.9.5 (issue #2143) — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/issues/2143
22. HTTP server source — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/blob/master/LibreHardwareMonitor.Windows.Forms/Utilities/HttpServer.cs
23. Перехід на PawnIO (PR #1857) — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/pull/1857
24. WMI output broken 0.9.5 — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/issues/2143
25. LibreHardwareMonitor 0.9.6 release — https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/tag/v0.9.6

**OpenHardwareMonitor**
26. OHM WMI provider documentation (PDF) — https://openhardwaremonitor.org/wordpress/wp-content/uploads/2011/04/OpenHardwareMonitor-WMI.pdf
27. OHM WMI класи та поля — https://github.com/openhardwaremonitor/openhardwaremonitor
28. Форк OHM — https://github.com/HardwareMonitor/openhardwaremonitor

**Afterburner / RTSS / інші**
29. MAHM shared memory header (v2.0) — https://forums.guru3d.com/threads/gpumonitor-sidebar-gadget.317912/page-4
30. MAHM entry layout (1324 bytes) і source IDs — https://github.com/Kinmanr-ops/DeskMon
31. RTSS shared memory header — https://github.com/RecursiveLife/RTSS_Crosshair/blob/master/RTSS_Crosshair_v2/RTSS_Crosshair/RTSSSharedMemory.h
32. Core Temp shared memory SDK — https://alcpu.com/CoreTemp/developers.html
33. HWMonitor PRO (немає публічного API) — https://www.cpuid.com/softwares/hwmonitor-pro.html
34. CapFrameX — https://github.com/CXWorld/CapFrameX

**Win32 / .NET**
35. Kernel object namespaces (`Global\`, `SeCreateGlobalPrivilege`) — https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces
36. File mapping security and access rights — https://learn.microsoft.com/en-us/windows/win32/memory/file-mapping-security-and-access-rights
37. CreateFileMappingW — https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-createfilemappingw
38. Native hosting .NET runtime — https://learn.microsoft.com/en-us/dotnet/core/tutorials/netcore-hosting
39. `netcorehost` crate — https://crates.io/crates/netcorehost

**Rust-крейти**
40. `sensorwatch` — https://crates.io/crates/sensorwatch
41. `sensorwatch` repo — https://github.com/lcjanke2020/sensorwatch
42. `rs-hwinfo2ha` — https://github.com/gaetanomatonti/rs-hwinfo2ha
43. `winmmf` — https://crates.io/crates/winmmf

---

*Підготовлено як research-артефакт для проєкту Oxide Monitor. Код застосунку тут не реалізується; документ описує лише контракти, формати й ризики.*
