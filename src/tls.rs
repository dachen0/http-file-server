use std::fs;
use std::io::{self, BufReader};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ConnectionTrafficSecrets, ExtractedSecrets, ProtocolVersion, ServerConfig};

// ---------------------------------------------------------------------------
// Linux kTLS constants (from <linux/tls.h> — not yet in the libc crate)
// ---------------------------------------------------------------------------

const SOL_TLS: libc::c_int = 282;
const TLS_TX: libc::c_int = 1;
const TLS_RX: libc::c_int = 2;
const TCP_ULP: libc::c_int = 31;

// ---------------------------------------------------------------------------
// kTLS crypto info structs — layout must match the kernel ABI exactly.
//
// Each struct begins with a 2-byte version and a 2-byte cipher_type, followed
// by the cipher-specific key material.  The kernel's setsockopt handler
// dispatches on cipher_type to determine the correct struct size.
// ---------------------------------------------------------------------------

#[repr(C)]
struct TlsCryptoInfoAesGcm128 {
    version: u16,
    cipher_type: u16,
    iv: [u8; 8],
    key: [u8; 16],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

#[repr(C)]
struct TlsCryptoInfoAesGcm256 {
    version: u16,
    cipher_type: u16,
    iv: [u8; 8],
    key: [u8; 32],
    salt: [u8; 4],
    rec_seq: [u8; 8],
}

#[repr(C)]
struct TlsCryptoInfoChacha20Poly1305 {
    version: u16,
    cipher_type: u16,
    iv: [u8; 12],
    key: [u8; 32],
    rec_seq: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm128>() == 40);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoAesGcm256>() == 56);
const _: () = assert!(std::mem::size_of::<TlsCryptoInfoChacha20Poly1305>() == 56);

// TLS version wire values
const TLS_1_2_VERSION: u16 = 0x0303;
const TLS_1_3_VERSION: u16 = 0x0304;

// Cipher type IDs from <linux/tls.h>
const TLS_CIPHER_AES_GCM_128: u16 = 51;
const TLS_CIPHER_AES_GCM_256: u16 = 52;
const TLS_CIPHER_CHACHA20_POLY1305: u16 = 54;

// ---------------------------------------------------------------------------
// kTLS setup
// ---------------------------------------------------------------------------

/// Enable kernel TLS offload on `fd` using the session secrets from `secrets`.
///
/// After this call, all `read()`/`write()`/`sendfile()` on the fd are
/// transparently decrypted/encrypted by the kernel — no TLS library code runs
/// in the hot I/O path.
pub fn setup_ktls(
    fd: RawFd,
    secrets: ExtractedSecrets,
    version: ProtocolVersion,
) -> io::Result<()> {
    let ver = match version {
        ProtocolVersion::TLSv1_2 => TLS_1_2_VERSION,
        ProtocolVersion::TLSv1_3 => TLS_1_3_VERSION,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported TLS version for kTLS",
            ))
        }
    };
    enable_tls_ulp(fd)?;
    let (tx_seq, tx) = secrets.tx;
    let (rx_seq, rx) = secrets.rx;
    install_direction(fd, TLS_TX, &tx, tx_seq, ver)?;
    install_direction(fd, TLS_RX, &rx, rx_seq, ver)?;
    Ok(())
}

fn enable_tls_ulp(fd: RawFd) -> io::Result<()> {
    // The kernel expects exactly the 3-byte string "tls" (no null terminator
    // in the optlen — the terminator is excluded from the count).
    let tls = b"tls\0";
    let ret = unsafe {
        libc::setsockopt(fd, libc::IPPROTO_TCP, TCP_ULP, tls.as_ptr().cast(), 3)
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn install_direction(
    fd: RawFd,
    direction: libc::c_int,
    secrets: &ConnectionTrafficSecrets,
    seq: u64,
    version: u16,
) -> io::Result<()> {
    let rec_seq = seq.to_be_bytes();
    match secrets {
        ConnectionTrafficSecrets::Aes128Gcm { key, iv } => {
            let iv_bytes = iv.as_ref(); // 12 bytes: [salt(4) | iv(8)]
            let info = TlsCryptoInfoAesGcm128 {
                version,
                cipher_type: TLS_CIPHER_AES_GCM_128,
                salt: iv_bytes[0..4].try_into().unwrap(),
                iv: iv_bytes[4..12].try_into().unwrap(),
                key: key.as_ref().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "AES-128 key must be 16 bytes")
                })?,
                rec_seq,
            };
            setsockopt_tls(fd, direction, &info)
        }
        ConnectionTrafficSecrets::Aes256Gcm { key, iv } => {
            let iv_bytes = iv.as_ref();
            let info = TlsCryptoInfoAesGcm256 {
                version,
                cipher_type: TLS_CIPHER_AES_GCM_256,
                salt: iv_bytes[0..4].try_into().unwrap(),
                iv: iv_bytes[4..12].try_into().unwrap(),
                key: key.as_ref().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "AES-256 key must be 32 bytes")
                })?,
                rec_seq,
            };
            setsockopt_tls(fd, direction, &info)
        }
        ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv } => {
            let info = TlsCryptoInfoChacha20Poly1305 {
                version,
                cipher_type: TLS_CIPHER_CHACHA20_POLY1305,
                iv: iv.as_ref().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "ChaCha20 IV must be 12 bytes")
                })?,
                key: key.as_ref().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "ChaCha20 key must be 32 bytes")
                })?,
                rec_seq,
            };
            setsockopt_tls(fd, direction, &info)
        }
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cipher suite not supported by kTLS",
        )),
    }
}

fn setsockopt_tls<T>(fd: RawFd, direction: libc::c_int, info: &T) -> io::Result<()> {
    let ret = unsafe {
        libc::setsockopt(
            fd,
            SOL_TLS,
            direction,
            (info as *const T).cast(),
            std::mem::size_of::<T>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Certificate / key loading
// ---------------------------------------------------------------------------

/// Build a `ServerConfig` from PEM certificate and key files.
///
/// Enables `enable_secret_extraction` so that kTLS can extract session keys
/// after the handshake.  Rustls defaults to TLS 1.2 and 1.3 only.
pub fn load_server_config(cert_path: &Path, key_path: &Path) -> io::Result<Arc<ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(fs::File::open(cert_path)?))
            .collect::<Result<_, _>>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    if certs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no certificates found in cert file",
        ));
    }

    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(fs::File::open(key_path)?))?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "no private key found in key file")
            })?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Required: allows dangerous_extract_secrets() to succeed after handshake.
    config.enable_secret_extraction = true;

    Ok(Arc::new(config))
}
