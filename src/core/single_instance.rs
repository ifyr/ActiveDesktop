use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;

const DEFAULT_NAME: PCWSTR = w!("ActiveDesktop_SingleInstance");

/// Process-wide single-instance guard backed by a named mutex.
///
/// Hold this for the lifetime of the run mode; dropping it releases the name.
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> windows::core::Result<Option<Self>> {
        Self::acquire_named(DEFAULT_NAME)
    }

    pub fn acquire_named(name: PCWSTR) -> windows::core::Result<Option<Self>> {
        unsafe {
            let handle = CreateMutexW(None, false, name)?;
            // CreateMutexW succeeds and returns a handle even when the name
            // already exists; the last-error tells us it was pre-existing.
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                Ok(None)
            } else {
                Ok(Some(Self { handle }))
            }
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SingleInstance;
    use windows::core::w;

    #[test]
    fn second_acquire_on_same_name_is_rejected() {
        let name = w!("ActiveDesktop_Test_SingleInstance");
        let first = SingleInstance::acquire_named(name).expect("first acquire");
        assert!(first.is_some(), "first acquire should own the name");
        let second = SingleInstance::acquire_named(name).expect("second acquire");
        assert!(second.is_none(), "second acquire must be rejected");
        drop(first);
        // After the owner is gone the name can be taken again.
        let third = SingleInstance::acquire_named(name).expect("third acquire");
        assert!(third.is_some(), "name should be free after owner drops");
    }
}
