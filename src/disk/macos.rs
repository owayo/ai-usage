//! macOS のマウント表とボリュームのリソース値を読む。`unsafe` はこのファイルに閉じ込める。

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use objc2::rc::autoreleasepool;
use objc2_foundation::{
    NSArray, NSNumber, NSString, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeAvailableCapacityKey, NSURLVolumeNameKey, NSURLVolumeTotalCapacityKey,
};

use super::{ReadError, Target, Volume};

pub(super) fn read(target: &Target) -> Result<Volume, ReadError> {
    match target {
        Target::Startup => query(Path::new("/"), &target.label()),
        Target::Mount(path) => {
            let mounts = mount_table().map_err(|error| {
                ReadError::Failed(format!("cannot read the mount table: {error}"))
            })?;
            // 同じ場所に重ねてマウントした場合は、後からマウントした方が見えている。
            let mount = mounts
                .iter()
                .rev()
                .find(|mount| mount.path == *path)
                .ok_or(ReadError::NotMounted)?;
            if !mount.local {
                return Err(ReadError::NotLocal);
            }
            query(path, &target.label())
        }
    }
}

/// マウント表の 1 行。
struct Mount {
    path: PathBuf,
    local: bool,
}

/// マウント表を読む。`MNT_NOWAIT` はカーネルが持っている情報を返すだけで、応答しない
/// ネットワークボリュームがあってもブロックしない。
fn mount_table() -> io::Result<Vec<Mount>> {
    // SAFETY: バッファに null と大きさ 0 を渡すと、getfsstat は件数だけを返し何も書かない。
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    // 2 回の呼び出しの間に増えたマウントの分だけ余裕を持たせる (入りきらない分は返らない)。
    let capacity = count as usize + 8;
    let mut entries = vec![libc::statfs::default(); capacity];
    let size =
        libc::c_int::try_from(capacity * size_of::<libc::statfs>()).map_err(io::Error::other)?;
    // SAFETY: entries は size バイトの初期化済みの領域で、getfsstat はその範囲にしか書かない。
    let filled = unsafe { libc::getfsstat(entries.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    if filled < 0 {
        return Err(io::Error::last_os_error());
    }
    entries.truncate(filled as usize);
    Ok(entries
        .iter()
        .map(|entry| Mount {
            path: c_path(&entry.f_mntonname),
            local: entry.f_flags & libc::MNT_LOCAL as u32 != 0,
        })
        .collect())
}

/// NUL で終わる固定長の C 文字列を、最初の NUL までのパスにする。
fn c_path(raw: &[libc::c_char]) -> PathBuf {
    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    PathBuf::from(OsStr::from_bytes(&bytes))
}

/// ボリュームの名前と容量を問い合わせる。
fn query(path: &Path, fallback_name: &str) -> Result<Volume, ReadError> {
    let failed = |message: String| ReadError::Failed(message);
    let path = path
        .to_str()
        .ok_or_else(|| failed("the mount point is not valid UTF-8".into()))?;
    // 問い合わせ中に作られる一時オブジェクトを、このスレッドの中で解放する。
    autoreleasepool(|_| {
        // SAFETY: Foundation が公開する不変の NSString 定数で、プロセスが終わるまで有効。
        let keys = unsafe {
            [
                NSURLVolumeNameKey,
                NSURLVolumeTotalCapacityKey,
                NSURLVolumeAvailableCapacityKey,
                NSURLVolumeAvailableCapacityForImportantUsageKey,
            ]
        };
        let [name_key, total_key, free_key, important_key] = keys;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let values = url
            .resourceValuesForKeys_error(&NSArray::from_slice(&keys))
            .map_err(|error| failed(error.localizedDescription().to_string()))?;
        let number = |key: &NSString| {
            values
                .objectForKey(key)
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .and_then(|value| u64::try_from(value.as_i64()).ok())
        };
        // ボリューム名は利用者が付けるので、端末を崩す制御文字を描画前に取り除く。
        let name = values
            .objectForKey(name_key)
            .and_then(|value| value.downcast::<NSString>().ok())
            .map(|value| {
                value
                    .to_string()
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
        let total = number(total_key).ok_or_else(|| failed("no capacity reported".into()))?;
        let free = number(free_key).ok_or_else(|| failed("no free space reported".into()))?;
        Volume::new(name, total, free, number(important_key)).map_err(failed)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_table_marks_the_root_volume_local() {
        let mounts = mount_table().unwrap();
        let root = mounts
            .iter()
            .find(|mount| mount.path == Path::new("/"))
            .unwrap();
        assert!(root.local);
    }

    #[test]
    fn c_path_stops_at_the_first_nul() {
        let raw = [
            b'/' as libc::c_char,
            b'V' as libc::c_char,
            0,
            b'x' as libc::c_char,
        ];
        assert_eq!(c_path(&raw), Path::new("/V"));
        assert_eq!(c_path(&[0; 4]), Path::new(""));
    }
}
