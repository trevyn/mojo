
use std::path::{Path, PathBuf};
use crate::{Error, file::MojoFile};
use crate::index::Index;
use crate::value::Value;
use crate::state::KVState;

pub struct BucketInner {
    name: String,
    root_path: PathBuf,
    index: Index,
    file_page_sz: usize,
    fmap: FileMap,
    is_dirty: bool,
    is_modified: bool,
    is_closed: bool,
    active_ver: u32,
}

impl BucketInner {
    fn active_file(&mut self, ver: u32) -> &mut MojoFile {
        self.fmap.file_mut(ver)
    }

    fn sync_index(&mut self, ver: u32) -> Result<(), Error> {

        let index_path = Bucket::index_path(&self.root_path, self.name.as_str(), ver);
        log::debug!("syncing index ver={} {:?}", ver, index_path);
        self.index.serialize_to_path(&index_path)?;
        log::debug!("syncing index ver={} done", ver);
        Ok(())
    }
}

pub struct Bucket {
    state: KVState,
    //inner: Arc<RwLock<BucketInner>>,
    inner: BucketInner,
    is_write: bool,
}

impl Bucket {
    fn with_inner(state: KVState, inner: BucketInner) -> Self {
        Bucket {
            state,
            //inner: Arc::new(RwLock::new(inner)),
            inner,
            is_write: false,
        }
    }

    pub fn set_writable(&mut self) {
        self.is_write = true
    }

    pub fn readonly(root_path: &Path, name: &str, ver: u32, state: KVState) -> Result<Bucket, Error> {
        log::debug!("bucket name={} readonly at ver={}", name, ver);

        let b = Self::load(root_path, name,  state, ver)?;
        Ok(b)
    }

    fn index_path(rootpath: &Path, name: &str, ver: u32) -> PathBuf {
        rootpath.join(&format!("{}_i.{}", name, ver))
    }

    pub fn get_key(&self, key: u32) -> Option<Value> {
        //let inner = self.inner.read();
        self.inner.index.get(key).map(|v| v.clone())
    }

    pub fn max_key(&self) ->  isize {
        //let inner = self.inner.read();
        self.inner.index.max_key()
    }

    pub fn is_modified(&self) ->  bool {
        //let inner = self.inner.read();
        self.inner.is_modified
    }

    pub fn writable(root_path: &Path, name: &str, state: KVState, load_ver: u32) -> Result<Bucket, Error> {
        log::debug!("mojo initing bucket pps={}", state.pps());

        let aver = state.active_ver();
        let index_path = Self::index_path(root_path, name, load_ver);

        let mut b = if index_path.exists() {
            log::debug!("bucket index for version={} exists", load_ver);
            Self::load(root_path, name, state, aver)?
        }else{
            log::debug!("creating new bucket at ver={}", aver);
            let b = Self::new(root_path, name, state)?;
            //b.inner.sync_index(aver)?;
            b
        };

        b.set_writable();

        log::debug!("mojo state={:?}", b.state);

        Ok(b)
    }

    pub fn load(root_path: &Path, name: &str, state: KVState, ver: u32) -> Result<Self, Error> {
        log::debug!("loading bucket={} version={}", name, ver);

        if ver < state.min_ver() || ver > state.active_ver() {
            return Err(Error::VersionNotFoundErr(ver));
        }

        let fmap = FileMap::init(root_path, name, state.min_ver(), state.active_ver())?;
        let mut index = Self::load_index(root_path, name, ver)?;
        index.set_active_ver(state.active_ver());

        let file_page_sz = state.page_size() as usize + MojoFile::header_len();

        let inner = BucketInner {
            name: name.to_owned(),
            root_path: root_path.to_owned(),
            index,
            file_page_sz,
            fmap,
            is_dirty: false,
            is_modified: false,
            is_closed: false,
            active_ver: state.active_ver(),
        };

        log::debug!("mojo load version done");
        Ok(Bucket::with_inner(state, inner))
    }

    pub fn load_index(root_path: &Path, name: &str, ver: u32) -> Result<Index, Error> {
        let index_path = Self::index_path(root_path, name, ver);
        if !index_path.exists() {
            return Err(Error::BucketNotAtVerErr(name.to_owned(), ver));
        }

        let index = Index::deserialize_from_path(&index_path)?;

        Ok(index)
    }

    pub fn load_index_header(root_path: &Path, name: &str, ver: u32) -> Result<(usize, usize, Index), Error> {
        let index_path = Self::index_path(root_path, name, ver);

        Index::deserialize_header_from_path(&index_path)
    }

    pub fn new(root_path: &Path, name: &str, state: KVState) -> Result<Self, Error> {
        log::debug!("creating new bucket name={} at ver={}", name, state.active_ver());

        let _ = std::fs::create_dir_all(root_path)?;

        let fmap =  FileMap::init(root_path, name, state.active_ver(), state.active_ver())?;

        let mut inner = BucketInner {
            name: name.to_owned(),
            root_path: root_path.to_owned(),
            index: Index::new(state.pps() as usize),
            file_page_sz: state.page_size() as usize + MojoFile::header_len(),
            fmap,
            is_dirty: false,
            is_modified: false,
            is_closed: false,
            active_ver: state.active_ver(),
        };

        inner.index.set_active_ver(state.active_ver());
        Ok(Bucket::with_inner(state, inner))
    }

    pub fn logical_size(&self) -> u64 {
        //let inner = self.inner.read();
        (self.state.page_size() as isize * (self.inner.index.max_key() + 1)) as u64
    }

    pub fn close(mut self) -> Result<(), Error> {
        //let mut inner = self.inner.write();
        if self.inner.is_closed {
            return Ok(())
        }

        self.inner.fmap.close()?;
        self.inner.is_closed = true;
        Ok(())
    }

    pub fn truncate(&mut self, new_sz: usize) -> Result<(), Error> {
        let _ = self.state.commit_lock.read();

        //let mut inner = self.inner.write();
        log::debug!("truncate bucket={} new_sz={}", self.inner.name, new_sz);
        let pages = new_sz/(self.state.page_size() as usize);
        //let real_sz = pages * self.file_page_sz;

        self.inner.index.truncate(pages as u32);
        self.inner.is_modified = true;
        //TODO: Delete blocks from file
        //self.active_file().truncate(real_sz)?;

        Ok(())
    }

    fn put_at(&mut self, key: u32, page_off: u64, buf: &[u8], val: &Value) -> Result<(), Error> {

        let mut off = val.get_off() as u64;
        off = off * self.inner.file_page_sz as u64;
        off += page_off;
        let file = self.inner.active_file(self.state.active_ver());
        file.write_buf_at(off, key, buf)?;

        Ok(())
    }

    pub fn put(&mut self, key: u32, page_off: u64, buf: &[u8]) -> Result<(), Error> {
        if !self.is_write {
            return Err(Error::BucketNotWritableErr);
        }

        if self.inner.active_ver < self.state.active_ver() {
            return Err(Error::VerNotWritable(self.inner.active_ver, self.state.active_ver()));
        }

        let _ = self.state.commit_lock.read();

        log::debug!("store put aver={} key={}, buflen={}", self.state.active_ver(), key, buf.len());

        let val_opt = self.get_value_opt(key).map(|v| v.clone());

        match val_opt {
            Some(val) => {
                //let mut inner = self.inner.write();

                log::debug!("store put value exists value={:?}", val);
                if val.get_ver() == self.state.active_ver() {
                    self.put_at(key, page_off, buf, &val)?;
                    self.inner.index.put(key, val.get_off());
                }else{
                    let file = self.inner.active_file(self.state.active_ver());
                    let write_off = file.write_buf(key, page_off, buf)?;
                    let block_no = (write_off/(self.inner.file_page_sz as u64)) as u32;
                    self.inner.index.put(key, block_no);
                }
                self.inner.is_dirty = true;
                self.inner.is_modified = true;
            },
            None => {
                //let mut inner = self.inner.write();
                
                let file = self.inner.active_file(self.state.active_ver());
                let write_off = file.write_buf(key, page_off, buf)?;
                let block_no = (write_off/(self.inner.file_page_sz as u64)) as u32;

                self.inner.index.put(key, block_no);
                self.inner.is_dirty = true;
                self.inner.is_modified = true;
                log::debug!("store put value not present. value={:?}", block_no);
            }
        }

        Ok(())
    }

    pub fn get(&self, key: u32, page_off: u64, out_buf: &mut [u8]) -> Result<usize, Error> {
        //let inner = self.inner.read();

        let value = self.get_value(key)?;

        let mut read_off = (value.get_off() as u64) * (self.inner.file_page_sz as u64);
        read_off += MojoFile::header_len() as u64 + page_off;
        let read_ver = value.get_ver();

        log::debug!("get name={} key={} value: {:?}", self.inner.name, key, value);
        let file = self.inner.fmap.file(read_ver);
        let n = file.read_buf_at(read_off, out_buf)?;
        log::debug!("get name={} key={} n={}", self.inner.name, key, n);

        Ok(n)
    }

    fn get_value_opt(&self, key: u32) -> Option<Value> {
        //let inner = self.inner.read();

        match self.inner.index.get(key) {
            None => {
                log::debug!("get_value_opt no slot key={}", key);
                return None
            }
            Some(val) => {
                if !val.is_allocated() {
                    log::debug!("get_value_opt allocated key={}", key);
                    return None
                }else{
                    return Some(val.clone())
                }
            }
        }
    }

    fn get_value(&self, key: u32) -> Result<Value, Error> {
        self.get_value_opt(key).ok_or(Error::KeyNotFoundErr(key))
    }

    pub (crate) fn sync_no_commit_lock(&mut self) -> Result<(), Error> {
        if !self.is_write {
            return Err(Error::StoreNotWritableErr);
        }

        //let mut inner = self.inner.write();

        log::debug!("syncing bucket={} at ver={} done", self.inner.name, self.state.active_ver());

        self.inner.active_file(self.state.active_ver()).sync()?;
        self.inner.sync_index(self.state.active_ver())?;
        self.inner.is_dirty = false;

        log::debug!("syncing done");
        Ok(())
    }

    pub fn sync(&mut self) -> Result<(), Error> {
        let _ = self.state.commit_lock.read();

        self.sync_no_commit_lock()
    }

    pub fn delete_ver(root_path: &Path, name: &str, ver: u32) -> Result<(), Error> {
        log::debug!("Deleting bucket name={} ver={}", name, ver);

        let index_path = Self::index_path(root_path, name, ver);
        log::debug!("removing index file={:?}", index_path);
        std::fs::remove_file(index_path)?;

        let data_path = FileMap::data_path(&root_path, name, ver);
        log::debug!("removing data file={:?}", data_path);
        std::fs::remove_file(data_path)?;

        Ok(())
    }

}


struct FileMap {
    fmap: Vec<MojoFile>,
    min_ver: u32,
}

impl FileMap {
    fn init(root_path: &Path, name: &str, min_ver: u32, active_ver: u32) -> Result<Self, Error> {
        let mut fmap = FileMap {
            fmap: Vec::new(),
            min_ver,
        };

        for ver in min_ver..=active_ver {
            fmap.add_file(root_path, name, ver)?;
        }

        Ok(fmap)
    }

    fn close(&mut self) -> Result<(), Error> {
        for f in &mut self.fmap {
            f.close()?;
        }
        Ok(())
    }

    fn data_path(root_path: &Path, name: &str, ver: u32) -> PathBuf {
        root_path.join(format!("{}_d.{}", name, ver))
    }

    fn add_file(&mut self, root_path: &Path, name: &str, ver: u32) -> Result<(), Error> {
        let ver_path = Self::data_path(root_path, name, ver);
        log::debug!("adding new file: {:?}", ver_path);

        let f = MojoFile::open(&ver_path, ver)?;

        self.fmap.push(f);
        Ok(())
    }

    fn file_mut(&mut self, ver: u32) -> &mut MojoFile {
        let i = ver - self.min_ver;
        &mut self.fmap[i as usize]
    }

    fn file(&self, ver: u32) -> &MojoFile {
        let i = ver - self.min_ver;
        &self.fmap[i as usize]
    }
}