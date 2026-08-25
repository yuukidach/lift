use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use nix::libc::{
    O_RDONLY, O_WRONLY, POSIX_SPAWN_CLOEXEC_DEFAULT, POSIX_SPAWN_SETPGROUP, c_char, pid_t,
    posix_spawn_file_actions_addopen, posix_spawn_file_actions_destroy,
    posix_spawn_file_actions_init, posix_spawn_file_actions_t, posix_spawnattr_destroy,
    posix_spawnattr_init, posix_spawnattr_setflags, posix_spawnattr_setpgroup, posix_spawnattr_t,
    posix_spawnp,
};
use tracing::error;

use crate::actor::broadcast::BroadcastEvent;
use crate::common::collections::{HashMap, HashSet};
use crate::ipc::subscriptions::CliSubscription;

pub trait CliExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        event: &BroadcastEvent,
        subscription: &CliSubscription,
    ) -> Result<i32, std::io::Error>;
}

pub struct DefaultCliExecutor;

impl DefaultCliExecutor {
    pub fn new() -> Self { Self {} }
}

fn environment_for_event(
    event: &BroadcastEvent,
) -> Result<HashMap<String, String>, serde_json::Error> {
    let mut env_vars = HashMap::default();
    match event {
        BroadcastEvent::WorkspaceChanged {
            workspace_id,
            workspace_name,
            space_id,
            display_uuid,
        } => {
            env_vars.insert("LIFT_EVENT_TYPE".into(), "workspace_changed".into());
            env_vars.insert("LIFT_WORKSPACE_ID".into(), workspace_id.to_string());
            env_vars.insert("LIFT_WORKSPACE_NAME".into(), workspace_name.clone());
            env_vars.insert("LIFT_SPACE_ID".into(), space_id.to_string());
            if let Some(display_uuid) = display_uuid.as_ref() {
                env_vars.insert("LIFT_DISPLAY_UUID".into(), display_uuid.clone());
            }
        }
        BroadcastEvent::WindowsChanged {
            workspace_id,
            workspace_name,
            windows,
            space_id,
            display_uuid,
        } => {
            env_vars.insert("LIFT_EVENT_TYPE".into(), "windows_changed".into());
            env_vars.insert("LIFT_WORKSPACE_ID".into(), workspace_id.to_string());
            env_vars.insert("LIFT_WORKSPACE_NAME".into(), workspace_name.clone());
            env_vars.insert("LIFT_WINDOW_COUNT".into(), windows.len().to_string());
            env_vars.insert("LIFT_WINDOWS".into(), windows.join(","));
            env_vars.insert("LIFT_SPACE_ID".into(), space_id.to_string());
            if let Some(display_uuid) = display_uuid.as_ref() {
                env_vars.insert("LIFT_DISPLAY_UUID".into(), display_uuid.clone());
            }
        }
        BroadcastEvent::WindowTitleChanged {
            window_id,
            workspace_id,
            workspace_index,
            workspace_name,
            previous_title,
            new_title,
            space_id,
            display_uuid,
        } => {
            env_vars.insert("LIFT_EVENT_TYPE".into(), "window_title_changed".into());
            env_vars.insert("LIFT_WINDOW_ID".into(), window_id.to_debug_string());
            env_vars.insert("LIFT_WORKSPACE_ID".into(), workspace_id.to_string());
            env_vars.insert("LIFT_WORKSPACE_NAME".into(), workspace_name.clone());
            if let Some(workspace_index) = workspace_index {
                env_vars.insert("LIFT_WORKSPACE_INDEX".into(), workspace_index.to_string());
            }
            env_vars.insert("LIFT_PREVIOUS_WINDOW_TITLE".into(), previous_title.clone());
            env_vars.insert("LIFT_WINDOW_TITLE".into(), new_title.clone());
            env_vars.insert("LIFT_SPACE_ID".into(), space_id.to_string());
            if let Some(display_uuid) = display_uuid.as_ref() {
                env_vars.insert("LIFT_DISPLAY_UUID".into(), display_uuid.clone());
            }
        }
        BroadcastEvent::StacksChanged {
            workspace_id,
            workspace_index,
            workspace_name,
            stacks,
            active_workspace_has_fullscreen,
            space_id,
            display_uuid,
        } => {
            env_vars.insert("LIFT_EVENT_TYPE".into(), "stacks_changed".into());
            env_vars.insert("LIFT_WORKSPACE_ID".into(), workspace_id.to_string());
            env_vars.insert("LIFT_WORKSPACE_NAME".into(), workspace_name.clone());
            if let Some(workspace_index) = workspace_index {
                env_vars.insert("LIFT_WORKSPACE_INDEX".into(), workspace_index.to_string());
            }
            env_vars.insert("LIFT_STACK_COUNT".into(), stacks.len().to_string());
            env_vars.insert(
                "LIFT_ACTIVE_WORKSPACE_HAS_FULLSCREEN".into(),
                active_workspace_has_fullscreen.to_string(),
            );
            env_vars.insert("LIFT_SPACE_ID".into(), space_id.to_string());
            if let Some(display_uuid) = display_uuid.as_ref() {
                env_vars.insert("LIFT_DISPLAY_UUID".into(), display_uuid.clone());
            }
        }
    }

    let event_json = serde_json::to_string(event)?;
    env_vars.insert("LIFT_EVENT_JSON".into(), event_json);
    Ok(env_vars)
}

impl CliExecutor for DefaultCliExecutor {
    fn execute(
        &self,
        event: &BroadcastEvent,
        subscription: &CliSubscription,
    ) -> Result<i32, std::io::Error> {
        let env_vars = match environment_for_event(event) {
            Ok(env) => env,
            Err(e) => {
                error!("Failed to serialize event for CLI executor: {}", e);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "serialization error",
                ));
            }
        };
        let event_json = env_vars["LIFT_EVENT_JSON"].clone();

        let command = subscription.command.clone();
        let mut args = subscription.args.clone();
        args.push(event_json.clone());

        let mut argv_storage: Vec<CString> = Vec::with_capacity(1 + args.len());
        argv_storage.push(CString::new(command).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "command contains NUL")
        })?);
        for a in args {
            argv_storage.push(CString::new(a.as_str()).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "arg contains NUL")
            })?);
        }
        let mut argv: Vec<*mut c_char> =
            argv_storage.iter_mut().map(|s| s.as_ptr() as *mut c_char).collect();
        argv.push(ptr::null_mut());

        let mut override_keys =
            HashSet::<Vec<u8>>::with_capacity_and_hasher(env_vars.len(), Default::default());
        for (k, _) in env_vars.clone() {
            override_keys.insert(k.as_bytes().to_vec());
        }
        let mut env_storage: Vec<CString> = Vec::new();
        for (k, v) in std::env::vars_os() {
            let kb = k.as_bytes().to_vec();
            if override_keys.contains(&kb) {
                continue;
            }
            let mut kv = kb;
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            env_storage.push(CString::new(kv).unwrap());
        }
        for (k, v) in env_vars {
            let mut kv = k.clone().into_bytes();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            env_storage.push(CString::new(kv).unwrap());
        }
        let mut envp: Vec<*mut c_char> =
            env_storage.iter_mut().map(|s| s.as_ptr() as *mut c_char).collect();
        envp.push(ptr::null_mut());

        let mut attr: posix_spawnattr_t = unsafe { std::mem::zeroed() };
        let rc_init = unsafe { posix_spawnattr_init(&mut attr) };
        if rc_init != 0 {
            return Err(std::io::Error::from_raw_os_error(rc_init));
        }

        let mut flags: i16 = 0;
        flags |= POSIX_SPAWN_CLOEXEC_DEFAULT as i16; // close-on-exec default
        flags |= POSIX_SPAWN_SETPGROUP as i16; // child in its own process group
        let rc_flags = unsafe { posix_spawnattr_setflags(&mut attr, flags) };
        if rc_flags != 0 {
            let _ = unsafe { posix_spawnattr_destroy(&mut attr) };
            return Err(std::io::Error::from_raw_os_error(rc_flags));
        }

        let rc_pgrp = unsafe { posix_spawnattr_setpgroup(&mut attr, 0) };
        if rc_pgrp != 0 {
            let _ = unsafe { posix_spawnattr_destroy(&mut attr) };
            return Err(std::io::Error::from_raw_os_error(rc_pgrp));
        }

        let mut fa: posix_spawn_file_actions_t = unsafe { std::mem::zeroed() };
        let rc_fa = unsafe { posix_spawn_file_actions_init(&mut fa) };
        if rc_fa != 0 {
            let _ = unsafe { posix_spawnattr_destroy(&mut attr) };
            return Err(std::io::Error::from_raw_os_error(rc_fa));
        }
        let devnull = CString::new("/dev/null").unwrap();
        unsafe {
            let _ = posix_spawn_file_actions_addopen(&mut fa, 0, devnull.as_ptr(), O_RDONLY, 0);
            let _ = posix_spawn_file_actions_addopen(&mut fa, 1, devnull.as_ptr(), O_WRONLY, 0);
            let _ = posix_spawn_file_actions_addopen(&mut fa, 2, devnull.as_ptr(), O_WRONLY, 0);
        }

        let mut child_pid: pid_t = 0;
        let rc = unsafe {
            posix_spawnp(
                &mut child_pid as *mut pid_t,
                argv_storage[0].as_ptr(),
                &fa as *const _,
                &attr as *const _,
                argv.as_mut_ptr(),
                envp.as_mut_ptr(),
            )
        };

        let _ = unsafe { posix_spawn_file_actions_destroy(&mut fa) };
        let _ = unsafe { posix_spawnattr_destroy(&mut attr) };

        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc));
        }

        crate::sys::dispatch::reap_on_exit_proc(child_pid);

        Ok(child_pid)
    }
}

pub fn execute_cli_subscription(event: &BroadcastEvent, subscription: &CliSubscription) {
    let exec = DefaultCliExecutor::new();
    let _ = exec.execute(event, subscription);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::WorkspaceId;
    use crate::sys::screen::SpaceId;

    #[test]
    fn event_environment_uses_lift_namespace() {
        let workspace_id = WorkspaceId(1);
        let event = BroadcastEvent::WorkspaceChanged {
            space_id: SpaceId::new(91),
            workspace_id,
            workspace_name: "code".into(),
            display_uuid: Some("display-a".into()),
        };
        let env = environment_for_event(&event).unwrap();

        assert_eq!(env["LIFT_EVENT_TYPE"], "workspace_changed");
        assert_eq!(env["LIFT_WORKSPACE_ID"], workspace_id.to_string());
        assert_eq!(env["LIFT_WORKSPACE_NAME"], "code");
        assert_eq!(env["LIFT_SPACE_ID"], "91");
        assert_eq!(env["LIFT_DISPLAY_UUID"], "display-a");
        assert!(env.contains_key("LIFT_EVENT_JSON"));
        let old_prefix = ["RIFT", "_"].concat();
        assert!(!env.keys().any(|key| key.starts_with(&old_prefix)));
    }
}
