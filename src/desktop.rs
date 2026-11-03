use eframe::egui;

pub fn icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}

pub fn identify() {
    // Stable shell identity: a changing progress title must not create a new taskbar group.
    let id: Vec<u16> = "HBSync.Desktop\0".encode_utf16().collect();
    unsafe {
        windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}

pub struct Instance(windows_sys::Win32::Foundation::HANDLE);

impl Instance {
    pub fn acquire() -> Option<Self> {
        Self::acquire_named("Local\\HBSync-single-instance")
    }

    fn acquire_named(name: &str) -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let name: Vec<u16> = format!("{name}\0").encode_utf16().collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn icon_is_valid_and_instance_lock_is_exclusive() {
        let icon = icon();
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon.rgba.chunks_exact(4).any(|p| p[3] == 0));
        let name = format!("Local\\OhMyBackup-test-{}", std::process::id());
        let first = Instance::acquire_named(&name).unwrap();
        assert!(Instance::acquire_named(&name).is_none());
        drop(first);
        assert!(Instance::acquire_named(&name).is_some());
    }
}
