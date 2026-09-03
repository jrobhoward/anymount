//! Phase 0 spike driver: mount a tiny in-memory tree and serve it.
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

        Self {
            nodes,
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
    let mountpoint = std::env::args().nth(1).ok_or(
        "usage: memfs <mountpoint>\n\
         hint: mkdir -p /tmp/anymount-demo && cargo run --example memfs -- /tmp/anymount-demo",
    )?;

    let mount = MountBuilder::new(&mountpoint)
        .fs_name("anymount-memfs")
        .mount(MemFs::new())?;

    println!("mounted at {}", mount.mountpoint().display());
    println!("try:  ls -lR {mountpoint}");
    println!("      cat {mountpoint}/hello.txt");
    println!("      sha256sum {mountpoint}/numbers.txt");
    println!("\npress Enter to unmount...");

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;

    mount.unmount()?;
    println!("unmounted");
    Ok(())
}
