//! Windows Task Scheduler COM backend (`ITaskService` / `ITaskDefinition`).

use std::ffi::c_void;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, VARIANT_BOOL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW,
};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskDefinition, ITaskFolder, ITaskService, TASK_ACTION_EXEC,
    TASK_CREATE_OR_UPDATE, TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_INTERACTIVE_TOKEN,
    TASK_RUNLEVEL_LUA, TASK_STATE_DISABLED, TASK_TRIGGER_LOGON, TaskScheduler,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, Interface, w};

use crate::{AUTOSTART_DELAY, AUTOSTART_DESCRIPTION, AutostartBackend, AutostartError, TaskSpec};

const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;
const HRESULT_FILE_NOT_FOUND: i32 = 0x8007_0002u32 as i32;
const LEGACY_RUN_KEY: windows::core::PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const LEGACY_RUN_VALUE: windows::core::PCWSTR = w!("Oxide Monitor");

/// Production backend: per-user task, interactive token, limited privileges.
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskSchedulerBackend;

impl AutostartBackend for TaskSchedulerBackend {
    fn ensure_task(&self, spec: &TaskSpec<'_>) -> Result<(), AutostartError> {
        let _apartment = ComApartment::enter("CoInitializeEx(TaskScheduler)")?;
        let (service, folder) = connect_task_service()?;
        let user = current_user_sid_string()?;
        let definition = build_definition(&service, &user, spec)?;

        unsafe {
            folder
                .RegisterTaskDefinition(
                    &BSTR::from(spec.name),
                    &definition,
                    TASK_CREATE_OR_UPDATE.0,
                    &VARIANT::default(),
                    &VARIANT::default(),
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &VARIANT::default(),
                )
                .map_err(|err| AutostartError::com("RegisterTaskDefinition", err))?;
        }
        Ok(())
    }

    fn delete_task(&self, name: &str) -> Result<(), AutostartError> {
        let _apartment = ComApartment::enter("CoInitializeEx(TaskScheduler)")?;
        let (_service, folder) = connect_task_service()?;
        match unsafe { folder.DeleteTask(&BSTR::from(name), 0) } {
            Ok(()) => Ok(()),
            Err(err) if err.code().0 == HRESULT_FILE_NOT_FOUND => Ok(()),
            Err(err) => Err(AutostartError::com("DeleteTask", err)),
        }
    }

    fn task_registered(&self, name: &str) -> Result<bool, AutostartError> {
        let _apartment = ComApartment::enter("CoInitializeEx(TaskScheduler)")?;
        let (_service, folder) = connect_task_service()?;
        match unsafe { folder.GetTask(&BSTR::from(name)) } {
            Ok(task) => {
                let state = unsafe { task.State() }
                    .map_err(|err| AutostartError::com("IRegisteredTask::State", err))?;
                Ok(state != TASK_STATE_DISABLED)
            }
            Err(err) if err.code().0 == HRESULT_FILE_NOT_FOUND => Ok(false),
            Err(err) => Err(AutostartError::com("GetTask", err)),
        }
    }

    fn remove_legacy_run(&self) -> Result<bool, AutostartError> {
        let mut key = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                LEGACY_RUN_KEY,
                Some(0),
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        if status != ERROR_SUCCESS {
            return Err(AutostartError::win32("RegOpenKeyExW(Run)", status.0));
        }

        let delete = unsafe { RegDeleteValueW(key, LEGACY_RUN_VALUE) };
        let _ = unsafe { RegCloseKey(key) };
        match delete {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            other => Err(AutostartError::win32("RegDeleteValueW(Run)", other.0)),
        }
    }
}

/// Balances `CoInitializeEx`; a thread already in another apartment keeps working.
struct ComApartment {
    release: bool,
}

impl ComApartment {
    fn enter(context: &'static str) -> Result<Self, AutostartError> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self { release: true })
        } else if hr.0 == RPC_E_CHANGED_MODE {
            // COM is already initialized as STA on this (UI) thread; Task Scheduler COM
            // is marshaled, so use the existing apartment and do not uninitialize it.
            Ok(Self { release: false })
        } else {
            Err(AutostartError::com(
                context,
                windows::core::Error::from_hresult(hr),
            ))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.release {
            unsafe { CoUninitialize() };
        }
    }
}

fn connect_task_service() -> Result<(ITaskService, ITaskFolder), AutostartError> {
    let service: ITaskService = unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_ALL) }
        .map_err(|err| AutostartError::com("CoCreateInstance(TaskScheduler)", err))?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }
        .map_err(|err| AutostartError::com("ITaskService::Connect", err))?;
    let folder = unsafe { service.GetFolder(&BSTR::from("\\")) }
        .map_err(|err| AutostartError::com("ITaskService::GetFolder", err))?;
    Ok((service, folder))
}

fn build_definition(
    service: &ITaskService,
    user: &str,
    spec: &TaskSpec<'_>,
) -> Result<ITaskDefinition, AutostartError> {
    let definition = unsafe { service.NewTask(0) }
        .map_err(|err| AutostartError::com("ITaskService::NewTask", err))?;

    let registration = unsafe { definition.RegistrationInfo() }
        .map_err(|err| AutostartError::com("ITaskDefinition::RegistrationInfo", err))?;
    unsafe {
        registration
            .SetDescription(&BSTR::from(AUTOSTART_DESCRIPTION))
            .map_err(|err| AutostartError::com("IRegistrationInfo::SetDescription", err))?;
        registration
            .SetAuthor(&BSTR::from("Oxide Monitor"))
            .map_err(|err| AutostartError::com("IRegistrationInfo::SetAuthor", err))?;
    }

    let principal = unsafe { definition.Principal() }
        .map_err(|err| AutostartError::com("ITaskDefinition::Principal", err))?;
    unsafe {
        principal
            .SetUserId(&BSTR::from(user))
            .map_err(|err| AutostartError::com("IPrincipal::SetUserId", err))?;
        principal
            .SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)
            .map_err(|err| AutostartError::com("IPrincipal::SetLogonType", err))?;
        principal
            .SetRunLevel(TASK_RUNLEVEL_LUA)
            .map_err(|err| AutostartError::com("IPrincipal::SetRunLevel", err))?;
    }

    let settings = unsafe { definition.Settings() }
        .map_err(|err| AutostartError::com("ITaskDefinition::Settings", err))?;
    unsafe {
        settings
            .SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)
            .map_err(|err| AutostartError::com("ITaskSettings::SetMultipleInstances", err))?;
        settings
            .SetDisallowStartIfOnBatteries(VARIANT_BOOL::from(false))
            .map_err(|err| {
                AutostartError::com("ITaskSettings::SetDisallowStartIfOnBatteries", err)
            })?;
        settings
            .SetStopIfGoingOnBatteries(VARIANT_BOOL::from(false))
            .map_err(|err| AutostartError::com("ITaskSettings::SetStopIfGoingOnBatteries", err))?;
        // PT0S disables the default three-day execution limit.
        settings
            .SetExecutionTimeLimit(&BSTR::from("PT0S"))
            .map_err(|err| AutostartError::com("ITaskSettings::SetExecutionTimeLimit", err))?;
        settings
            .SetStartWhenAvailable(VARIANT_BOOL::from(true))
            .map_err(|err| AutostartError::com("ITaskSettings::SetStartWhenAvailable", err))?;
        settings
            .SetEnabled(VARIANT_BOOL::from(true))
            .map_err(|err| AutostartError::com("ITaskSettings::SetEnabled", err))?;
    }

    let triggers = unsafe { definition.Triggers() }
        .map_err(|err| AutostartError::com("ITaskDefinition::Triggers", err))?;
    let trigger = unsafe { triggers.Create(TASK_TRIGGER_LOGON) }
        .map_err(|err| AutostartError::com("ITriggerCollection::Create(logon)", err))?;
    let logon: ILogonTrigger = trigger
        .cast()
        .map_err(|err| AutostartError::com("ITrigger::cast(ILogonTrigger)", err))?;
    unsafe {
        logon
            .SetUserId(&BSTR::from(user))
            .map_err(|err| AutostartError::com("ILogonTrigger::SetUserId", err))?;
        logon
            .SetDelay(&BSTR::from(AUTOSTART_DELAY))
            .map_err(|err| AutostartError::com("ILogonTrigger::SetDelay", err))?;
    }

    let actions = unsafe { definition.Actions() }
        .map_err(|err| AutostartError::com("ITaskDefinition::Actions", err))?;
    let action = unsafe { actions.Create(TASK_ACTION_EXEC) }
        .map_err(|err| AutostartError::com("IActionCollection::Create(exec)", err))?;
    let exec: IExecAction = action
        .cast()
        .map_err(|err| AutostartError::com("IAction::cast(IExecAction)", err))?;
    unsafe {
        exec.SetPath(&BSTR::from(wide_path(spec.exe)))
            .map_err(|err| AutostartError::com("IExecAction::SetPath", err))?;
        exec.SetArguments(&BSTR::from(spec.arguments))
            .map_err(|err| AutostartError::com("IExecAction::SetArguments", err))?;
        exec.SetWorkingDirectory(&BSTR::from(wide_path(spec.working_dir)))
            .map_err(|err| AutostartError::com("IExecAction::SetWorkingDirectory", err))?;
    }

    Ok(definition)
}

fn wide_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// `S-1-5-21-...` for the current user, used for the principal and the logon trigger.
fn current_user_sid_string() -> Result<String, AutostartError> {
    let token = {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(|err| AutostartError::com("OpenProcessToken", err))?;
        token
    };

    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(token) };
        return Err(AutostartError::Message(
            "GetTokenInformation(TokenUser) returned 0 bytes".into(),
        ));
    }
    let mut buffer = vec![0u8; needed as usize];
    let info = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    };
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(token) };
    info.map_err(|err| AutostartError::com("GetTokenInformation(TokenUser)", err))?;

    let token_user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_ptr = windows::core::PWSTR::null();
    unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid_ptr) }
        .map_err(|err| AutostartError::com("ConvertSidToStringSidW", err))?;
    if sid_ptr.is_null() {
        return Err(AutostartError::Message(
            "ConvertSidToStringSidW returned null".into(),
        ));
    }
    let sid = unsafe {
        let mut len = 0usize;
        while *sid_ptr.0.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid_ptr.0, len));
        let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            sid_ptr.0.cast::<c_void>(),
        )));
        text
    };
    Ok(sid)
}
