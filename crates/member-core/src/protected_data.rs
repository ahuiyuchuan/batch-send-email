//! 当前 Windows 用户的 DPAPI 边界。作者：ahui。
//! 业务模块必须使用独立 purpose；加密失败不降级，解密后的原生缓冲区及时擦除。

/// SMTP 使用固定用途的 DPAPI 附加熵，保留已有授权码密文格式；QQ 登录不再持久化。
#[derive(Clone, Copy)]
pub(crate) enum Purpose {
    SmtpCredential,
}

impl Purpose {
    #[cfg(windows)]
    fn entropy(self) -> &'static [u8] {
        match self {
            Self::SmtpCredential => b"com.ahui.qq-group-exporter.smtp-credential.v1",
        }
    }
}

/// 仅返回不可展示的底层失败标记；业务模块负责生成不含密码或系统细节的可读错误。
pub(crate) fn protect(input: &[u8], purpose: Purpose) -> Result<Vec<u8>, ()> {
    crypt(input, purpose, false)
}

/// 返回的明文字节必须立即放入 Zeroizing 容器，禁止写入日志、SQLite 或前端响应。
pub(crate) fn unprotect(input: &[u8], purpose: Purpose) -> Result<Vec<u8>, ()> {
    crypt(input, purpose, true)
}

#[cfg(windows)]
fn crypt(input: &[u8], purpose: Purpose, decrypt: bool) -> Result<Vec<u8>, ()> {
    use std::ffi::c_void;

    #[repr(C)]
    struct Blob {
        length: u32,
        data: *mut u8,
    }
    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    if input.is_empty() || input.len() > 128 * 1024 {
        return Err(());
    }
    let input = Blob {
        length: input.len() as u32,
        data: input.as_ptr() as *mut u8,
    };
    let entropy_bytes = purpose.entropy();
    let entropy = Blob {
        length: entropy_bytes.len() as u32,
        data: entropy_bytes.as_ptr() as *mut u8,
    };
    let mut output = Blob {
        length: 0,
        data: std::ptr::null_mut(),
    };
    // UI_FORBIDDEN 禁止意外弹出系统密码框；不指定 LOCAL_MACHINE，限定当前 Windows 用户。
    let success = unsafe {
        if decrypt {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut output,
            )
        }
    };
    if success == 0 {
        return Err(());
    }
    // DPAPI 成功时返回可读、由 LocalAlloc 分配的缓冲区；复制后擦除明文并成对释放。
    let result = unsafe {
        let bytes = std::slice::from_raw_parts_mut(output.data, output.length as usize);
        let result = bytes.to_vec();
        if decrypt {
            zeroize::Zeroize::zeroize(bytes);
        }
        LocalFree(output.data.cast());
        result
    };
    Ok(result)
}

/// 非 Windows 开发环境不提供明文替代方案，调用方必须明确说明无法安全保存凭据。
#[cfg(not(windows))]
fn crypt(_input: &[u8], _purpose: Purpose, _decrypt: bool) -> Result<Vec<u8>, ()> {
    Err(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn smtp_credentials_preserve_existing_purpose_and_reject_damage() {
        let plain = b"synthetic-purpose-isolation";
        let mut cipher = protect(plain, Purpose::SmtpCredential).unwrap();
        assert_eq!(unprotect(&cipher, Purpose::SmtpCredential).unwrap(), plain);
        let last = cipher.len() - 1;
        cipher[last] ^= 1;
        assert!(unprotect(&cipher, Purpose::SmtpCredential).is_err());
    }
}
