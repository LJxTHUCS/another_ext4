//! To make `Ext4FuseFs` behave like `RefFS`, these FUSE interfaces
//! need to be implemented.
//!
//! init destroy lookup forget getattr setattr readlink mknod mkdir
//! unlink rmdir symlink rename link open read write flush release
//! fsync opendir readdir releasedir fsyncdir statfs setxattr getxattr
//! listxattr removexattr access create getlk ioctl
//!
//! Rust crate `fuser` doesn't have the detailed explantion of these interfaces.
//! See `fuse_lowlevel_ops` in C FUSE library for details.
//! https://libfuse.github.io/doxygen/structfuse__lowlevel__ops.html

use super::common::{
    sys_time2second, time_or_now2second, translate_attr, translate_ftype, DirHandler, FileHandler,
};
use ext4_rs::{DirEntry, ErrCode, Ext4, Ext4Error, InodeMode, OpenFlags};
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyEmpty, ReplyEntry,
    ReplyOpen, ReplyWrite, Request,
};
use std::ffi::OsStr;
use std::time::Duration;

type FId = u64;

pub struct Ext4FuseFs {
    fs: Ext4,
    files: Vec<FileHandler>,
    next_fid: FId,
    dirs: Vec<DirHandler>,
    next_did: FId,
}

impl Ext4FuseFs {
    pub fn new(fs: Ext4) -> Self {
        Self {
            fs,
            files: Vec::new(),
            next_fid: 0,
            dirs: Vec::new(),
            next_did: 0,
        }
    }

    /// Add a file handler to file list
    fn add_file(&mut self, inode: u32, flags: OpenFlags) -> FId {
        self.files
            .push(FileHandler::new(self.next_did, inode, flags));
        self.next_fid += 1;
        self.next_fid - 1
    }

    fn release_file(&mut self, fh: FId) {
        self.files.retain(|f| f.id != fh);
    }

    /// Add a directory handler to directory list
    fn add_dir(&mut self, entries: Vec<DirEntry>) -> FId {
        self.dirs.push(DirHandler::new(self.next_did, entries));
        self.next_did += 1;
        self.next_did - 1
    }

    fn release_dir(&mut self, did: FId) {
        self.dirs.retain(|d| d.id != did);
    }

    fn get_attr(&self, inode: u32) -> Result<FileAttr, Ext4Error> {
        match self.fs.getattr(inode) {
            Ok(attr) => Ok(translate_attr(attr)),
            Err(e) => Err(e),
        }
    }
}

impl Filesystem for Ext4FuseFs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.fs.lookup(parent as u32, name.to_str().unwrap()) {
            Ok(inode_id) => reply.entry(&get_ttl(), &self.get_attr(inode_id).unwrap(), 0),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyAttr) {
        match self.get_attr(ino as u32) {
            Ok(attr) => reply.attr(&get_ttl(), &attr),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        match self.fs.setattr(
            ino as u32,
            mode.map(|m| InodeMode::from_bits_truncate(m as u16)),
            uid,
            gid,
            size,
            atime.map(|t| time_or_now2second(t)),
            mtime.map(|t| time_or_now2second(t)),
            ctime.map(|t| sys_time2second(t)),
            crtime.map(|t| sys_time2second(t)),
        ) {
            Ok(_) => reply.attr(&get_ttl(), &self.get_attr(ino as u32).unwrap()),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.fs.create(
            parent as u32,
            name.to_str().unwrap(),
            InodeMode::from_bits_truncate(mode as u16),
        ) {
            Ok(ino) => {
                let fid = self.add_file(ino, OpenFlags::from_bits_truncate(flags as u32));
                reply.created(&get_ttl(), &self.get_attr(ino).unwrap(), 0, fid, 0);
            }
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let attr = self.get_attr(ino as u32);
        match attr {
            Ok(attr) => {
                if attr.kind != FileType::RegularFile {
                    return reply.error(ErrCode::EISDIR as i32);
                }
            }
            Err(e) => return reply.error(e.code() as i32),
        }
        let fid = self.add_file(ino as u32, OpenFlags::from_bits_truncate(flags as u32));
        reply.opened(fid, 0);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        // let fh = match self.files.iter_mut().find(|f| f.id == fh) {
        //     Some(f) => f,
        //     None => return reply.error(ErrCode::ENOENT as i32),
        // };
        let mut data = vec![0; size as usize];
        match self.fs.read(ino as u32, offset as usize, &mut data) {
            Ok(sz) => reply.data(&data[..sz]),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        match self.fs.write(ino as u32, offset as usize, data) {
            Ok(sz) => reply.written(sz as u32),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.release_file(fh);
        reply.ok();
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        match self
            .fs
            .link(ino as u32, newparent as u32, newname.to_str().unwrap())
        {
            Ok(_) => reply.entry(&get_ttl(), &self.get_attr(ino as u32).unwrap(), 0),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        match self.fs.unlink(parent as u32, name.to_str().unwrap()) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        match self.fs.rename(
            parent as u32,
            name.to_str().unwrap(),
            newparent as u32,
            newname.to_str().unwrap(),
        ) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        match self.fs.mkdir(
            parent as u32,
            name.to_str().unwrap(),
            InodeMode::from_bits_truncate(mode as u16),
        ) {
            Ok(ino) => reply.entry(&get_ttl(), &self.get_attr(ino).unwrap(), 0),
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn opendir(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.fs.list(ino as u32) {
            Ok(entries) => {
                let fh = self.add_dir(entries);
                reply.opened(fh, 0);
            }
            Err(e) => reply.error(e.code() as i32),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        let dir = self.dirs.iter_mut().find(|d| d.id == fh);
        match dir {
            Some(dir) => {
                loop {
                    let entry = dir.next_entry();
                    if entry.is_none() {
                        break;
                    }
                    let entry = entry.unwrap();
                    if reply.add(
                        ino,
                        dir.cur as i64,
                        translate_ftype(self.fs.getattr(entry.inode()).unwrap().ftype),
                        entry.name().unwrap(),
                    ) {
                        break;
                    }
                }
                reply.ok();
            }
            None => reply.error(-1),
        }
    }

    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        self.release_dir(fh);
        reply.ok();
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        match self.fs.rmdir(parent as u32, name.to_str().unwrap()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.code() as i32),
        }
    }
}

fn get_ttl() -> Duration {
    Duration::from_secs(1)
}