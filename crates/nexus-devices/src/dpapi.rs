//! Protects secrets at rest: the node's private key and backup keys.
//!
//! On Windows the key is encrypted with DPAPI for the current user, so copying
//! the data folder to another account or computer does not copy the identity.
//! Other platforms (development only) store it unencrypted.

pub const SCHEME: &str = if cfg!(windows) { "dpapi" } else { "plain" };

#[cfg(windows)]
pub fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData};

    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `data`, which outlives the call; DPAPI
    // allocates `output` with LocalAlloc and we free it below.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: on success `output` holds `cbData` bytes owned by us.
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(bytes)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData};

    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe { LocalFree(output.pbData as _) };
    Ok(bytes)
}

#[cfg(not(windows))]
pub fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    Ok(data.to_vec())
}
