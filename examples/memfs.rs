//! Mounts a tiny in-memory tree and serves it.
//!
//! ```text
//! /
//! ├── hello.txt
//! ├── numbers.txt
//! └── subdir/
//!     └── nested.txt
//! ```
//!
//! Run with:
//!
//! ```sh
//! mkdir -p /tmp/anymount-demo
//! cargo run --example memfs -- /tmp/anymount-demo
//! ```
//!
//! Then, from another shell, verify with real tools rather than trusting the
//! process output: `ls -lR`, `cat`, `find`, and `sha256sum`.
//!
//! Pass `--open` to also pop a file-manager window at the mount root, via the
//! `opener` crate (a dev-dependency; nothing in the library itself does this —
//! see README.md's "Opening the mount in a file manager").

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anymount::{
    DirEntry, FileAttr, FileHandle, FileKind, FsError, Ino, MountBuilder, ROOT_INO, ReadOnlyFs,
    Result,
};

enum Node {
    Dir(BTreeMap<OsString, Ino>),
    File(Vec<u8>),
}

struct MemFs {
    nodes: HashMap<Ino, Node>,
    parents: HashMap<Ino, Ino>,
    open_files: Mutex<HashMap<FileHandle, Ino>>,
    next_handle: AtomicU64,
}

impl MemFs {
    fn new() -> Self {
        let mut nodes = HashMap::new();

        nodes.insert(Ino(2), Node::File(b"Hello from anymount!\n".to_vec()));
        nodes.insert(
            Ino(3),
            Node::File(
                (1..=100)
                    .map(|n| format!("{n}\n"))
                    .collect::<String>()
                    .into_bytes(),
            ),
        );
        nodes.insert(Ino(5), Node::File(b"I am nested.\n".to_vec()));

        let mut subdir = BTreeMap::new();
        subdir.insert(OsString::from("nested.txt"), Ino(5));
        nodes.insert(Ino(4), Node::Dir(subdir));

        let mut root = BTreeMap::new();
        root.insert(OsString::from("hello.txt"), Ino(2));
        root.insert(OsString::from("numbers.txt"), Ino(3));
        root.insert(OsString::from("subdir"), Ino(4));
        nodes.insert(ROOT_INO, Node::Dir(root));

        let mut parents = HashMap::new();
        parents.insert(ROOT_INO, ROOT_INO);
        parents.insert(Ino(4), ROOT_INO);

        Self {
            nodes,
            parents,
            open_files: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    fn node(&self, ino: Ino) -> Result<&Node> {
        self.nodes.get(&ino).ok_or(FsError::NotFound)
    }

    fn attr(&self, ino: Ino) -> Result<FileAttr> {
        Ok(match self.node(ino)? {
            Node::Dir(_) => FileAttr::dir(ino),
            Node::File(data) => FileAttr::file(ino, data.len() as u64),
        })
    }
}

impl ReadOnlyFs for MemFs {
    fn lookup(&self, parent: Ino, name: &OsStr) -> Result<FileAttr> {
        // FUSE's kernel client resolves `.`/`..` itself and never sends
        // these names here; NFS has no such cache and issues real wire
        // `LOOKUP` calls for both. See `ReadOnlyFs`'s "Inode lifetime" docs.
        if name == OsStr::new(".") {
            return self.attr(parent);
        }
        if name == OsStr::new("..") {
            let p = *self.parents.get(&parent).ok_or(FsError::NotFound)?;
            return self.attr(p);
        }
        let Node::Dir(children) = self.node(parent)? else {
            return Err(FsError::NotADirectory);
        };
        let ino = *children.get(name).ok_or(FsError::NotFound)?;
        self.attr(ino)
    }

    fn getattr(&self, ino: Ino) -> Result<FileAttr> {
        self.attr(ino)
    }

    fn readdir(&self, ino: Ino, offset: u64) -> Result<Vec<DirEntry>> {
        let Node::Dir(children) = self.node(ino)? else {
            return Err(FsError::NotADirectory);
        };
        children
            .iter()
            .skip(offset as usize)
            .map(|(name, &child)| {
                Ok(DirEntry {
                    ino: child,
                    name: name.clone(),
                    kind: match self.node(child)? {
                        Node::Dir(_) => FileKind::Directory,
                        Node::File(_) => FileKind::File,
                    },
                })
            })
            .collect()
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        if matches!(self.node(ino)?, Node::Dir(_)) {
            return Err(FsError::IsADirectory);
        }
        let fh = FileHandle(self.next_handle.fetch_add(1, Ordering::Relaxed));
        self.open_files
            .lock()
            .map_err(|_| FsError::Other("open_files mutex poisoned".into()))?
            .insert(fh, ino);
        Ok(fh)
    }

    fn read_at(&self, fh: FileHandle, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let ino = *self
            .open_files
            .lock()
            .map_err(|_| FsError::Other("open_files mutex poisoned".into()))?
            .get(&fh)
            .ok_or(FsError::InvalidArgument)?;

        let Node::File(data) = self.node(ino)? else {
            return Err(FsError::IsADirectory);
        };

        let start = (offset as usize).min(data.len());
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn release(&self, fh: FileHandle) -> Result<()> {
        self.open_files
            .lock()
            .map_err(|_| FsError::Other("open_files mutex poisoned".into()))?
            .remove(&fh);
        Ok(())
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mountpoint = args.next().ok_or(
        "usage: memfs <mountpoint> [--open]\n\
         hint: mkdir -p /tmp/anymount-demo && cargo run --example memfs -- /tmp/anymount-demo",
    )?;
    let open = args.any(|a| a == "--open");

    let mount = MountBuilder::new(&mountpoint)
        .fs_name("anymount-memfs")
        .mount(MemFs::new())?;

    println!("mounted at {}", mount.mountpoint().display());
    println!("try:  ls -lR {mountpoint}");
    println!("      cat {mountpoint}/hello.txt");
    println!("      sha256sum {mountpoint}/numbers.txt");

    if open {
        // `anymount` only ever mounts at the path the caller gave it — there
        // is no OS-injected relocation to account for — so the mountpoint
        // returned here is exactly what a file manager needs to point at.
        // Opening it is the caller's job, not the library's; `opener` is a
        // dev-dependency of this example alone.
        if let Err(e) = opener::open(mount.mountpoint()) {
            eprintln!("could not open a file manager window: {e}");
        }
    }

    println!("\npress Enter to unmount...");

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;

    mount.unmount()?;
    println!("unmounted");
    Ok(())
}
