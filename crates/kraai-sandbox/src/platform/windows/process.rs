mod attributes;
mod command_line;
mod pipes;
mod process_wait;

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::SandboxError;
use crate::config::PreparedCommand;

const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Child {
    job: OwnedHandle,
    process: OwnedHandle,
    pub(crate) stdout: Option<tokio::fs::File>,
    pub(crate) stderr: Option<tokio::fs::File>,
}

impl Child {
    #[expect(
        unsafe_code,
        reason = "querying a process exit status requires Windows APIs"
    )]
    pub(crate) async fn wait(&mut self) -> Result<ExitStatus, SandboxError> {
        process_wait::wait(&self.process)
            .await
            .map_err(|error| SandboxError::Wait(error.to_string()))?;
        let mut code = 0;
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(SandboxError::Wait(io::Error::last_os_error().to_string()));
        }
        self.kill_tree().await?;
        Ok(ExitStatus::from_raw(code))
    }

    pub(crate) async fn terminate(&mut self) -> Result<(), SandboxError> {
        self.kill_tree().await?;
        self.wait().await?;
        Ok(())
    }

    /// Waits for exit because termination is asynchronous and a dying process can still
    /// write files.
    #[expect(
        unsafe_code,
        reason = "Windows process trees are terminated through their job object"
    )]
    async fn kill_tree(&self) -> Result<(), SandboxError> {
        let failed = |action: &str| {
            SandboxError::Wait(format!(
                "unable to {action} process job: {}",
                io::Error::last_os_error()
            ))
        };
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(failed("terminate"));
        }
        let deadline = Instant::now() + TERMINATION_TIMEOUT;
        loop {
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&raw mut accounting).cast(),
                    std::mem::size_of_val(&accounting) as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(failed("query"));
            }
            if accounting.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(SandboxError::Wait(format!(
                    "sandboxed processes were still running {} seconds after termination",
                    TERMINATION_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}

pub(crate) fn spawn(command: &mut PreparedCommand) -> Result<Child, SandboxError> {
    crate::process_spawn::with_lock(|| spawn_inner(command)).map_err(|error| SandboxError::Spawn {
        executable: command.executable.to_string_lossy().into_owned(),
        message: error.to_string(),
    })
}

#[expect(
    unsafe_code,
    reason = "Windows sandboxed process creation uses STARTUPINFOEX and owned native handles"
)]
fn spawn_inner(command: &mut PreparedCommand) -> io::Result<Child> {
    let application = command_line::application(&command.executable)?;
    let mut arguments = command_line::arguments(&command.executable, &command.args)?;
    let environment = command_line::environment(&command.environment)?;
    let cwd = command_line::wide(command.cwd.as_os_str())?;
    let job = create_job()?;
    let io = pipes::Pipes::new()?;
    let inheritance = pipes::InheritedHandles::new(&command.private_ipc_handles)?;
    let mut handles = vec![
        io.stdin.as_raw_handle(),
        io.stdout_write.as_raw_handle(),
        io.stderr_write.as_raw_handle(),
    ];
    handles.extend(
        command
            .private_ipc_handles
            .iter()
            .map(AsRawHandle::as_raw_handle),
    );
    let jobs = [job.as_raw_handle()];
    let mut security = command
        .windows_sandbox
        .as_ref()
        .map(super::Sandbox::security_capabilities);
    if let Some((capabilities, entries)) = security.as_mut() {
        capabilities.Capabilities = if entries.is_empty() {
            std::ptr::null_mut()
        } else {
            entries.as_mut_ptr()
        };
    }
    let package_policy = 1_u32;
    let mut attributes = attributes::Attributes::new(if security.is_some() { 4 } else { 2 })?;
    attributes.add_slice(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    attributes.add_slice(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    if let Some((security, _)) = &security {
        attributes.add(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, security)?;
        attributes.add(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &package_policy,
        )?;
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = io.stdin.as_raw_handle();
    startup.StartupInfo.hStdOutput = io.stdout_write.as_raw_handle();
    startup.StartupInfo.hStdError = io.stderr_write.as_raw_handle();
    startup.lpAttributeList = attributes.as_ptr();
    let mut information = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            arguments.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
    drop(attributes);
    drop(inheritance);
    command.private_ipc_handles.clear();
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(Child {
        job,
        process,
        stdout: Some(tokio::fs::File::from_std(std::fs::File::from(
            io.stdout_read,
        ))),
        stderr: Some(tokio::fs::File::from_std(std::fs::File::from(
            io.stderr_read,
        ))),
    })
}

#[expect(
    unsafe_code,
    reason = "job handles provide automatic process-tree cleanup on failure and drop"
)]
fn create_job() -> io::Result<OwnedHandle> {
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}
