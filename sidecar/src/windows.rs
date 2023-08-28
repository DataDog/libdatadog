use crate::config::Config;
#[cfg(feature = "tracing")]
use crate::enable_tracing;
use crate::setup::pid_shm_path;
use crate::{config, enter_listener_loop};
use datadog_ipc::platform::{
    named_pipe_name_from_raw_handle, FileBackedHandle, MappedMem, NamedShmHandle,
};
use futures::FutureExt;
use manual_future::ManualFuture;
use spawn_worker::SpawnWorker;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::process::Stdio;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::select;
use windows::Wdk::Storage::FileSystem::{NtSetInformationFile, FILE_COMPLETION_INFORMATION};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::WindowsProgramming::FILE_INFORMATION_CLASS;

#[no_mangle]
pub extern "C" fn ddog_daemon_entry_point() {
    #[cfg(feature = "tracing")]
    enable_tracing().ok();
    let now = Instant::now();

    let pid = unsafe { libc::getpid() };

    if let Some(handle) = spawn_worker::recv_passed_handle() {
        let mut shm = match named_pipe_name_from_raw_handle(handle.as_raw_handle())
            .ok_or(io::Error::from(io::ErrorKind::InvalidInput))
            .and_then(|name| NamedShmHandle::create(pid_shm_path(&name), 4))
            .and_then(FileBackedHandle::map)
        {
            Ok(ok) => ok,
            Err(err) => {
                tracing::error!("Couldn't store pid to shared memory: {err}");
                return;
            }
        };
        shm.as_slice_mut().copy_from_slice(&pid.to_ne_bytes());

        tracing::info!("Starting sidecar, pid: {}", pid);

        let acquire_listener = move || unsafe {
            // This code ensures there's no completion port used for this handle - apparently pinned completion ports are preserved across process inheritance.
            // If we don't remove the completion port, the usage of the NamePipeServer will just silently hang indefinitely.
            // This restricts ourselves to minimum Windows 8.1
            // The alternative would be manually creating the NamedPipeServer handle in the spawning
            // process instead of using tokio code. And avoiding a registration being ever created for it.
            let completion_info = FILE_COMPLETION_INFORMATION {
                Port: Default::default(),
                Key: null_mut(),
            };

            #[allow(non_upper_case_globals)]
            const FileReplaceCompletionInformation: i32 = 61;

            let mut io_status_block = MaybeUninit::uninit();
            NtSetInformationFile(
                HANDLE(handle.as_raw_handle() as isize),
                io_status_block.as_mut_ptr(),
                &completion_info as *const FILE_COMPLETION_INFORMATION as *const core::ffi::c_void,
                std::mem::size_of_val(&completion_info) as u32,
                FILE_INFORMATION_CLASS(FileReplaceCompletionInformation),
            )?;

            let (closed_future, close_completer) = ManualFuture::new();
            let close_completer = Arc::from(Mutex::new(Some(close_completer)));
            let pipe = NamedPipeServer::from_raw_handle(handle.into_raw_handle())?;

            let cancel = move || {
                if let Some(completer) = close_completer.lock().unwrap().take() {
                    tokio::spawn(completer.complete(()));
                }
            };

            // We pass the shm to ensure we drop the shm handle with the pid immediately after cancellation
            // To avoid actual race conditions
            Ok((
                |handler| accept_socket_loop(pipe, closed_future, handler, shm),
                cancel,
            ))
        };

        if let Err(err) = enter_listener_loop(acquire_listener) {
            tracing::error!("Error: {err}")
        }
    }

    tracing::info!(
        "shutting down sidecar, pid: {}, total runtime: {:.3}s",
        pid,
        now.elapsed().as_secs_f64()
    )
}

async fn accept_socket_loop(
    mut pipe: NamedPipeServer,
    cancellation: ManualFuture<()>,
    handler: Box<dyn Fn(NamedPipeServer)>,
    _: MappedMem<NamedShmHandle>,
) -> io::Result<()> {
    let name = named_pipe_name_from_raw_handle(pipe.as_raw_handle())
        .ok_or(io::Error::from(io::ErrorKind::InvalidInput))?;

    let cancellation = cancellation.shared();
    loop {
        select! {
            _ = cancellation.clone() => break,
            result = pipe.connect() => result?,
        }
        let connected_pipe = pipe;
        pipe = ServerOptions::new().create(&name)?;
        handler(connected_pipe);
    }
    // drops pipe and shm here
    Ok(())
}

pub fn setup_daemon_process(
    listener: &NamedPipeServer,
    cfg: Config,
    spawn_cfg: &mut SpawnWorker,
) -> io::Result<()> {
    spawn_cfg.pass_handle(unsafe { OwnedHandle::from_raw_handle(listener.as_raw_handle()) });

    match cfg.log_method {
        config::LogMethod::File(path) => {
            let file = File::options()
                .write(true)
                .append(true)
                .truncate(false)
                .create(true)
                .open(path)?;
            let (out, err) = (Stdio::from(file.try_clone()?), Stdio::from(file));
            spawn_cfg.stdout(out);
            spawn_cfg.stderr(err);
        }
        _ => {}
    }

    Ok(())
}
