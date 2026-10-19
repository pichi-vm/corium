// SPDX-FileCopyrightText: Advanced Micro Devices, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::qemu::READY;
use super::{Harness, hash, input, run, text};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

const BLOCK: usize = 4096;
const MIB: u64 = 1024 * 1024;

fn copy(source: &Path, root: &Path) {
    let dest = root.join(source.strip_prefix("/").unwrap());
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::copy(source, dest).unwrap();
}
fn libraries(binary: &Path, root: &Path) {
    for word in text(Command::new("ldd").arg(binary)).split_whitespace() {
        if word.starts_with('/') {
            copy(Path::new(word), root);
        }
    }
}
fn executable(path: &str, root: &Path) {
    copy(Path::new(path), root);
    libraries(Path::new(path), root);
}

pub(crate) fn root(h: &Harness, name: &str) -> PathBuf {
    let root = h.work.join(name);
    for dir in [
        "sbin",
        "proc",
        "sys",
        "dev",
        "run",
        "etc",
        "var",
        "tmp",
        "fixture",
        "usr/bin",
        "etc/systemd/system",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    executable("/usr/lib/systemd/systemd", &root);
    if Path::new("/usr/lib/systemd/systemd-executor").exists() {
        executable("/usr/lib/systemd/systemd-executor", &root);
    }
    for name in [
        "libmount.so.1",
        "libselinux.so.1",
        "libkmod.so.2",
        "libcap.so.2",
        "libseccomp.so.2",
        "libacl.so.1",
    ] {
        let cache = text(Command::new("ldconfig").arg("-p"));
        if let Some(line) = cache
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{name} ")))
        {
            let path = line.split(" => ").nth(1).unwrap();
            executable(path, &root);
        }
    }
    executable("/usr/bin/echo", &root);
    executable("/usr/bin/sha256sum", &root);
    symlink("/usr/lib/systemd/systemd", root.join("sbin/init")).unwrap();
    fs::write(root.join("etc/machine-id"), "").unwrap();
    fs::write(root.join("etc/os-release"), "ID=corium-test\nNAME=Corium\n").unwrap();
    fs::write(
        root.join("etc/systemd/system/default.target"),
        "[Unit]\nRequires=boot-complete.service\n",
    )
    .unwrap();
    service(&root, false);
    root
}
fn service(root: &Path, validate: bool) {
    let check = if validate {
        "ExecStart=/usr/bin/sha256sum -c /fixture/checksums\n"
    } else {
        ""
    };
    fs::write(root.join("etc/systemd/system/boot-complete.service"), format!(
        "[Unit]\nDescription=Validate Corium root\nDefaultDependencies=no\nSuccessAction=poweroff-immediate\nFailureAction=poweroff-immediate\n[Service]\nType=oneshot\n{check}ExecStart=/usr/bin/echo {READY}\nStandardOutput=tty\nStandardError=tty\nTTYPath=/dev/console\n")).unwrap();
}

fn walk(path: &Path, entries: &mut Vec<PathBuf>) {
    let mut children: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    children.sort();
    for child in children {
        entries.push(child.clone());
        if child.symlink_metadata().unwrap().is_dir() {
            walk(&child, entries);
        }
    }
}

pub(crate) fn initramfs(
    h: &Harness,
    name: &str,
    empty: bool,
    bad: bool,
    compression: &str,
) -> PathBuf {
    let stage = h.work.join(format!("initrd-{name}"));
    fs::create_dir(&stage).unwrap();
    fs::copy(&h.binary, stage.join("init")).unwrap();
    let sources = if empty { Vec::new() } else { h.modules.clone() };
    let mut inventory = Vec::new();
    for source in sources {
        let suffix = source.extension().unwrap().to_str().unwrap();
        let bytes = match suffix {
            "xz" => run(Command::new("xz").args(["-d", "-c"]).arg(&source)).stdout,
            "gz" => run(Command::new("gzip").args(["-d", "-c"]).arg(&source)).stdout,
            "zst" => run(Command::new("zstd").args(["-q", "-d", "-c"]).arg(&source)).stdout,
            "ko" => fs::read(&source).unwrap(),
            _ => panic!("unknown installed module format: {}", source.display()),
        };
        let relative = source
            .strip_prefix(format!("/lib/modules/{}", h.release))
            .or_else(|_| source.strip_prefix(format!("/usr/lib/modules/{}", h.release)))
            .unwrap();
        let basename = relative
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .split(".ko")
            .next()
            .unwrap();
        let ext = if compression == "none" {
            "ko".into()
        } else {
            format!("ko.{compression}")
        };
        let dest = stage
            .join("usr/lib/modules")
            .join(&h.release)
            .join(relative)
            .with_file_name(format!("{basename}.{ext}"));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let bytes = match compression {
            "none" => bytes,
            "xz" => input(Command::new("xz").args(["--check=crc32", "-c"]), &bytes).stdout,
            "gz" => input(Command::new("gzip").args(["-n", "-c"]), &bytes).stdout,
            "zst" => input(Command::new("zstd").args(["-q", "-c"]), &bytes).stdout,
            _ => panic!("unsupported compression"),
        };
        fs::write(&dest, bytes).unwrap();
        inventory.push(json!({"source":source,"source_sha256":hash(&source),"packed":dest.strip_prefix(&stage).unwrap(),"packed_sha256":hash(&dest)}));
    }
    if bad {
        let path = stage
            .join("usr/lib/modules")
            .join(&h.release)
            .join("broken.ko");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "not a module").unwrap();
    }
    let mut entries = Vec::new();
    walk(&stage, &mut entries);
    let mut names = b".\0".to_vec();
    for path in entries {
        names.extend_from_slice(
            path.strip_prefix(&stage)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes(),
        );
        names.push(0);
    }
    let bytes = input(
        Command::new("cpio")
            .current_dir(&stage)
            .args(["--null", "-o", "-H", "newc", "--quiet"]),
        &names,
    )
    .stdout;
    let archive = stage.with_extension("cpio");
    fs::write(&archive, bytes).unwrap();
    fs::write(
        stage.with_extension("modules.json"),
        serde_json::to_vec_pretty(&inventory).unwrap(),
    )
    .unwrap();
    archive
}

#[derive(Debug)]
pub(crate) struct Scute {
    pub(crate) data: PathBuf,
    pub(crate) hashes: PathBuf,
    pub(crate) root: String,
    pub(crate) payload: u64,
}
#[derive(Debug)]
pub(crate) struct Disk {
    pub(crate) path: PathBuf,
    pub(crate) root: String,
    pub(crate) offsets: Vec<(u64, u64)>,
}

// Format authority: carapace rev 99caf7941ac8930a5761f6f843b6d8e53dd89587,
// docs/spec.md (4096-byte chunks, SHA-256 salt-prefix parent linkage).
// Independent writer for the kernel's persistent dm-snapshot format: one
// header chunk followed by metadata areas of 256 little-endian exceptions.
fn snapshot(current: &Path, previous: Option<&Path>, dest: &Path) -> u64 {
    let size = fs::metadata(current).unwrap().len();
    let mut source = File::open(current).unwrap();
    let mut parent = previous.map(|p| File::open(p).unwrap());
    if let Some(parent) = &parent {
        assert_eq!(parent.metadata().unwrap().len(), size);
    }
    let mut out = File::create(dest).unwrap();
    let mut header = [0u8; BLOCK];
    for (i, value) in [0x7041_6e53u32, 1, 1, 8].iter().enumerate() {
        header[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    out.write_all(&header).unwrap();
    let mut exceptions = Vec::new();
    let mut area = 1u64;
    let mut count = 0;
    for index in 0..size.div_ceil(BLOCK as u64) {
        let mut data = [0u8; BLOCK];
        let n = (size - index * BLOCK as u64).min(BLOCK as u64) as usize;
        source.read_exact(&mut data[..n]).unwrap();
        let mut old = [0u8; BLOCK];
        if let Some(parent) = &mut parent {
            parent.read_exact(&mut old[..n]).unwrap();
        }
        if data == old {
            continue;
        }
        let chunk = area + 1 + exceptions.len() as u64;
        out.seek(SeekFrom::Start(chunk * BLOCK as u64)).unwrap();
        out.write_all(&data).unwrap();
        exceptions.push((index, chunk));
        count += 1;
        if exceptions.len() == 256 {
            metadata(&mut out, area, &exceptions);
            exceptions.clear();
            area += 257;
        }
    }
    metadata(&mut out, area, &exceptions);
    out.set_len((area + 1 + exceptions.len() as u64 + 1) * BLOCK as u64)
        .unwrap();
    count * BLOCK as u64
}
fn metadata(out: &mut File, area: u64, entries: &[(u64, u64)]) {
    let mut bytes = [0u8; BLOCK];
    for (i, (old, new)) in entries.iter().enumerate() {
        bytes[i * 16..i * 16 + 8].copy_from_slice(&old.to_le_bytes());
        bytes[i * 16 + 8..i * 16 + 16].copy_from_slice(&new.to_le_bytes());
    }
    out.seek(SeekFrom::Start(area * BLOCK as u64)).unwrap();
    out.write_all(&bytes).unwrap();
}
fn scute(
    work: &Path,
    current: &Path,
    previous: Option<&Path>,
    index: usize,
    parent: &str,
) -> Scute {
    let data = work.join(format!("scute-{index}.cow"));
    let hashes = work.join(format!("scute-{index}.hash"));
    let payload = snapshot(current, previous, &data);
    let output = text(
        Command::new("veritysetup")
            .arg("format")
            .arg(&data)
            .arg(&hashes)
            .args([
                "--hash=sha256",
                "--uuid=11111111-2222-3333-4444-777777777777",
                "--data-block-size=4096",
                "--hash-block-size=4096",
                &format!("--salt={parent}"),
            ]),
    );
    let root = output
        .lines()
        .find_map(|line| line.strip_prefix("Root hash:"))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(root.len(), 64);
    Scute {
        data,
        hashes,
        root,
        payload,
    }
}
fn uuid(hex: &str) -> String {
    assert_eq!(hex.len(), 32);
    format!(
        "{}{}{}{}-{}{}-{}{}-{}-{}",
        &hex[6..8],
        &hex[4..6],
        &hex[2..4],
        &hex[0..2],
        &hex[10..12],
        &hex[8..10],
        &hex[14..16],
        &hex[12..14],
        &hex[16..20],
        &hex[20..32]
    )
}

pub(crate) fn pack(work: &Path, name: &str, scutes: &[Scute], omit: Option<usize>) -> Disk {
    let path = work.join(format!("{name}.gpt"));
    let mut table = String::from(
        "label: gpt\nlabel-id: 11111111-2222-3333-4444-666666666666\nsector-size: 512\ntable-length: 128\n",
    );
    let mut sector = 2048u64;
    let mut parts = Vec::new();
    let mut offsets = Vec::new();
    for (i, scute) in scutes.iter().enumerate() {
        let mut pair = Vec::new();
        for (source, id) in [
            (&scute.data, &scute.root[..32]),
            (&scute.hashes, &scute.root[32..]),
        ] {
            pair.push(sector * 512);
            let sectors = fs::metadata(source).unwrap().len().div_ceil(512);
            if Some(i) != omit {
                writeln!(table, "start={sector},size={sectors},uuid={}", uuid(id)).unwrap();
                parts.push((source, sector, uuid(id)));
            }
            sector = (sector + sectors).div_ceil(2048) * 2048;
        }
        offsets.push((pair[0], pair[1]));
    }
    File::create(&path)
        .unwrap()
        .set_len((sector + 2048) * 512)
        .unwrap();
    input(Command::new("sfdisk").arg(&path), table.as_bytes());
    let mut disk = OpenOptions::new().write(true).open(&path).unwrap();
    for (source, sector, _) in &parts {
        disk.seek(SeekFrom::Start(sector * 512)).unwrap();
        std::io::copy(&mut File::open(source).unwrap(), &mut disk).unwrap();
    }
    run(Command::new("sfdisk").arg("--verify").arg(&path));
    let listing: serde_json::Value =
        serde_json::from_str(&text(Command::new("sfdisk").arg("--json").arg(&path))).unwrap();
    let entries = listing["partitiontable"]["partitions"].as_array().unwrap();
    assert_eq!(entries.len(), parts.len());
    for (entry, (source, start, id)) in entries.iter().zip(&parts) {
        assert_eq!(entry["uuid"].as_str().unwrap().to_lowercase(), *id);
        assert_eq!(entry["start"].as_u64().unwrap(), *start);
        assert_eq!(
            entry["size"].as_u64().unwrap(),
            fs::metadata(source).unwrap().len().div_ceil(512)
        );
    }
    fs::write(
        work.join(format!("{name}.partitions.json")),
        serde_json::to_vec_pretty(&listing).unwrap(),
    )
    .unwrap();
    Disk {
        path,
        root: scutes.last().unwrap().root.clone(),
        offsets,
    }
}

fn filesystem(root: &Path, path: &Path, size: u64) {
    let epoch = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let mut paths = vec![root.to_owned()];
    walk(root, &mut paths);
    for path in paths {
        if !path.symlink_metadata().unwrap().file_type().is_symlink() {
            File::open(path)
                .unwrap()
                .set_times(FileTimes::new().set_accessed(epoch).set_modified(epoch))
                .unwrap();
        }
    }
    File::create(path).unwrap().set_len(size).unwrap();
    run(Command::new("mkfs.ext4")
        .args([
            "-q",
            "-F",
            "-b",
            "4096",
            "-U",
            "11111111-2222-3333-4444-555555555555",
            "-E",
            "lazy_itable_init=0,lazy_journal_init=0,hash_seed=11111111-2222-3333-4444-888888888888",
            "-d",
        ])
        .arg(root)
        .arg(path));
}
fn check(path: &Path) {
    run(Command::new("e2fsck").arg("-fn").arg(path));
}

pub(crate) fn base(h: &Harness, root: &Path, name: &str, squash: bool) -> Disk {
    let work = h.work.join(name);
    fs::create_dir(&work).unwrap();
    let raw = work.join("filesystem");
    if squash {
        run(Command::new("mksquashfs").arg(root).arg(&raw).args([
            "-noappend",
            "-quiet",
            "-processors",
            "1",
        ]));
    } else {
        filesystem(root, &raw, 64 * MIB);
        check(&raw);
    }
    let scute = scute(&work, &raw, None, 0, &"00".repeat(32));
    pack(&work, "disk", &[scute], None)
}

// Validates offline edits by reading back the exact file, since debugfs can
// report an individual command error while returning a successful exit code.
fn put(raw: &Path, source: &Path, guest: &str, replace: bool) {
    if replace {
        run(Command::new("debugfs")
            .arg("-w")
            .arg("-R")
            .arg(format!("rm {guest}"))
            .arg(raw));
    }
    run(Command::new("debugfs")
        .arg("-w")
        .arg("-R")
        .arg(format!("write {} {guest}", source.display()))
        .arg(raw));
    let output = run(Command::new("debugfs")
        .arg("-R")
        .arg(format!("cat {guest}"))
        .arg(raw));
    assert_eq!(
        format!("{:x}", Sha256::digest(&output.stdout)),
        hash(source),
        "debugfs edit failed: {guest}"
    );
}

pub(crate) fn layered(h: &Harness, name: &str, depth: usize) -> (Disk, Vec<Scute>) {
    assert!(depth == 3 || depth == 50);
    let work = h.work.join(name);
    fs::create_dir(&work).unwrap();
    let root = root(h, &format!("{name}-root"));
    service(&root, true);
    let mut sums = String::new();
    for index in 0..depth {
        let bytes = format!("scute {index}: deterministic surviving sentinel\n");
        writeln!(
            sums,
            "{:x}  /fixture/sentinel-{index}",
            Sha256::digest(bytes.as_bytes())
        )
        .unwrap();
    }
    let overlap = format!("last writer is scute {}\n", depth - 1);
    writeln!(
        sums,
        "{:x}  /fixture/overlap",
        Sha256::digest(overlap.as_bytes())
    )
    .unwrap();
    fs::write(root.join("fixture/checksums"), &sums).unwrap();
    fs::write(
        root.join("fixture/sentinel-0"),
        "scute 0: deterministic surviving sentinel\n",
    )
    .unwrap();
    fs::write(root.join("fixture/overlap"), "last writer is scute 0\n").unwrap();
    let mut staged = Vec::new();
    walk(&root, &mut staged);
    let inventory: Vec<_> = staged.iter().filter(|p| p.symlink_metadata().unwrap().is_file()).map(|p| json!({"path":p.strip_prefix(&root).unwrap(),"sha256":hash(p),"bytes":p.metadata().unwrap().len()})).collect();
    let mut contents_bytes: u64 = inventory.iter().map(|v| v["bytes"].as_u64().unwrap()).sum();
    fs::write(
        work.join("guest-inputs.json"),
        serde_json::to_vec_pretty(&inventory).unwrap(),
    )
    .unwrap();
    let raw = work.join("current.ext4");
    filesystem(&root, &raw, if depth == 50 { 1536 * MIB } else { 64 * MIB });
    check(&raw);
    let mut scutes = vec![scute(&work, &raw, None, 0, &"00".repeat(32))];
    let previous = work.join("previous.ext4");
    let mut recipe = Vec::new();
    for index in 1..depth {
        fs::copy(&raw, &previous).unwrap();
        let sentinel = work.join("sentinel");
        fs::write(
            &sentinel,
            format!("scute {index}: deterministic surviving sentinel\n"),
        )
        .unwrap();
        put(
            &raw,
            &sentinel,
            &format!("/fixture/sentinel-{index}"),
            false,
        );
        fs::write(&sentinel, format!("last writer is scute {index}\n")).unwrap();
        put(&raw, &sentinel, "/fixture/overlap", true);
        // 44 small 4 MiB increments + five 160 MiB increments = 976 MiB,
        // plus the bootable base and filesystem metadata: roughly 1 GiB.
        let mib = if depth == 3 {
            0
        } else if index % 10 == 9 {
            160
        } else {
            4
        };
        if mib != 0 {
            let payload = work.join("payload");
            let mut file = File::create(&payload).unwrap();
            for block in 0..mib * 256 {
                let digest =
                    Sha256::digest(format!("corium-fixture-v1:{index}:{block}").as_bytes());
                let mut bytes = [0; BLOCK];
                for chunk in bytes.as_chunks_mut::<32>().0 {
                    chunk.copy_from_slice(&digest);
                }
                file.write_all(&bytes).unwrap();
            }
            drop(file);
            put(&raw, &payload, &format!("/fixture/payload-{index}"), false);
        }
        contents_bytes +=
            format!("scute {index}: deterministic surviving sentinel\n").len() as u64 + mib * MIB;
        contents_bytes += format!("last writer is scute {index}\n").len() as u64
            - format!("last writer is scute {}\n", index - 1).len() as u64;
        check(&raw);
        let next = scute(
            &work,
            &raw,
            Some(&previous),
            index,
            &scutes.last().unwrap().root,
        );
        recipe.push(json!({"index":index,"bulk_mib":mib,"payload_bytes":next.payload}));
        scutes.push(next);
    }
    fs::remove_file(&previous).unwrap();
    println!(
        "Built {depth} scutes; changed payload {} bytes",
        scutes.iter().map(|s| s.payload).sum::<u64>()
    );
    let disk = pack(&work, "disk", &scutes, None);
    let stats: Vec<_> = scutes.iter().enumerate().map(|(i,s)| json!({"index":i,"root":s.root,"changed_bytes":s.payload,"snapshot_bytes":fs::metadata(&s.data).unwrap().len(),"verity_bytes":fs::metadata(&s.hashes).unwrap().len()})).collect();
    fs::write(work.join("fixture.json"),serde_json::to_vec_pretty(&json!({"recipe_version":1,"increments":recipe,"scutes":stats,"logical_capacity":fs::metadata(&raw).unwrap().len(),"final_file_contents_bytes":contents_bytes,"disk_bytes":fs::metadata(&disk.path).unwrap().len(),"disk_sha256":hash(&disk.path),"checksums":sums})).unwrap()).unwrap();
    (disk, scutes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delta_preserves_zero_overwrites_and_terminal_area() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        let cow = dir.path().join("cow");
        fs::write(&old, vec![1; BLOCK * 256]).unwrap();
        fs::write(&new, vec![0; BLOCK * 256]).unwrap();
        assert_eq!(snapshot(&new, Some(&old), &cow), 256 * BLOCK as u64);
        let bytes = fs::read(&cow).unwrap();
        assert_eq!(
            u64::from_le_bytes(bytes[BLOCK + 8..BLOCK + 16].try_into().unwrap()),
            2
        );
        assert_eq!(
            u64::from_le_bytes(
                bytes[BLOCK + 255 * 16..BLOCK + 255 * 16 + 8]
                    .try_into()
                    .unwrap()
            ),
            255
        );
        assert!(bytes[258 * BLOCK..259 * BLOCK].iter().all(|b| *b == 0));
        assert!(bytes[2 * BLOCK..258 * BLOCK].iter().all(|b| *b == 0));
        assert_eq!(snapshot(&new, None, &cow), 0);
    }
    #[test]
    fn delta_omits_unchanged_blocks_and_preserves_partial_area() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        let cow = dir.path().join("cow");
        let original = vec![7; BLOCK * 4];
        let mut changed = original.clone();
        changed[BLOCK..BLOCK * 2].fill(0);
        changed[BLOCK * 3..].fill(9);
        fs::write(&old, original).unwrap();
        fs::write(&new, changed).unwrap();
        assert_eq!(snapshot(&new, Some(&old), &cow), 2 * BLOCK as u64);
        let bytes = fs::read(cow).unwrap();
        let entry =
            |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        assert_eq!((entry(BLOCK), entry(BLOCK + 8)), (1, 2));
        assert_eq!((entry(BLOCK + 16), entry(BLOCK + 24)), (3, 3));
        assert!(bytes[BLOCK + 32..BLOCK * 2].iter().all(|b| *b == 0));
        assert!(bytes[BLOCK * 2..BLOCK * 3].iter().all(|b| *b == 0));
        assert!(bytes[BLOCK * 3..BLOCK * 4].iter().all(|b| *b == 9));
    }

    #[test]
    fn partuuid_uses_guid_byte_order() {
        assert_eq!(
            uuid("000102030405060708090a0b0c0d0e0f"),
            "03020100-0504-0706-0809-0a0b0c0d0e0f"
        );
    }
}
