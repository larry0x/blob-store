# blob-store

A content-addressed file store with a database catalog, for Rust. It names each file by the hash of its content. It keeps a record of every file in a database, and it keeps the records and the files in sync: if the catalog has a record, the file exists.

## Usage

```rust
let store = BlobStore::<_, MyFormat>::open("/path/to/blobs", Sqlite::new(conn)?)?;

let staged = store.save(&bytes)?;      // into tmp/, hashed and sniffed
let landed = store.land(staged)?;      // renamed into place, fsynced
store.write(|tx| {
    tx.insert(landed, SystemTime::now())?;
    // The app's own rows go in the same transaction, through tx.inner().
    Ok::<_, blob_store::Error>(())
})?;
```

## Layout on disk

A blob lives at `{root}/{AA}/{BB}/{hash}.{ext}`:

- `hash` is the lowercase hex digest of the file's content.
- `AA`, `BB`, … are the first `N` bytes of the digest, one directory level per byte. `N` is a const generic, with a default of 2.
- `ext` is the extension of the file's format, which the app sniffs from the content.

The root also holds these entries:

- `tmp/` holds files that are still being received.
- `layout.toml` records the hasher and `N`. `open` writes it on first use and checks it after that. A mismatch is an error.
- `lock` carries an exclusive file lock while a store is open. A second process that opens the same root gets an error.

## Types

### Hasher

```rust
pub trait Hasher: Default + Send + 'static {
    const NAME: &'static str;
    const LEN: usize;
    type Output: HashOutput;
    fn update(&mut self, bytes: &[u8]);
    fn finalize(self) -> Self::Output;
    fn digest(bytes: &[u8]) -> Self::Output; // provided
}
```

The crate ships two hashers. Each one outputs a `Digest<32>`, a fixed-size byte array shown as lowercase hex.

- `Blake3` is behind the `hasher-blake3` feature. With that feature on, it is the default type parameter of the store.
- `Sha256` is behind the `hasher-sha256` feature.

No hasher is on by default, so an app turns on at least one. The crate ships no hasher with known collision attacks, such as MD5 or SHA-1. With one, two different files could share one name.

### Format

```rust
pub trait Format: Copy + Eq + Debug + Send + Sync + 'static {
    fn sniff(head: &[u8]) -> Option<Self>;
    fn extension(&self) -> &'static str;
    fn from_extension(extension: &str) -> Option<Self>;
}
```

`sniff` sees the first `SNIFF_LEN` bytes of the file, which is 8,192. Each app defines one enum with every format it accepts. One store holds blobs of all those formats. The format comes from the content, never from the caller. So a hash always maps to one path.

#### Presets

The `format-presets` feature defines nine common formats. Each one is a unit type that implements `Format` on its own, and the `Preset` trait:

| Preset | Extension | Media type |
|---|---|---|
| `Jpeg` | `jpg` | `image/jpeg` |
| `Png` | `png` | `image/png` |
| `Gif` | `gif` | `image/gif` |
| `Webp` | `webp` | `image/webp` |
| `Webm` | `webm` | `video/webm` |
| `Mp4` | `mp4` | `video/mp4` |
| `Mov` | `mov` | `video/quicktime` |
| `Gzip` | `gz` | `application/gzip` |
| `Pdf` | `pdf` | `application/pdf` |

The signature checks come from the `infer` crate.

Two signatures can match one file. For example, a PDF header may sit anywhere in the first 1,024 bytes, so a PNG can contain one. `identify` settles this: it tries the presets in the order of the table, so signatures at a fixed offset win over the PDF search.

The `formats!` macro builds an enum out of presets, with any number of them:

```rust
blob_store::formats! {
    #[derive(Serialize, Deserialize)]
    pub enum Media { Jpeg, Png, Webp, Gif, Mp4, Webm, Mov }
}
```

- Each variant must be named after a preset.
- The macro derives `Clone`, `Copy`, `Debug`, `PartialEq`, `Eq`, and `Hash`. Other attributes on the enum and its variants pass through.
- Its `sniff` first asks `identify` which preset a file is. It then accepts the file only if that preset is a variant. So a file of another format is rejected, whatever the order of the variants.
- The enum also gets `ALL`, every variant in the order written, and `media_type()`.

Rust has no variadic generics, so a generic `Union<A, B, C, …>` can't take any number of formats. The macro gives that, and keeps named variants that a `match` can check for completeness.

### Catalog

```rust
pub trait Catalog<F: Format, H: Hasher> {
    type Tx<'t>;
    type Error: std::error::Error + Send + Sync + 'static;

    fn write<T, E: From<Self::Error>>(
        &self,
        f: impl FnOnce(&Self::Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E>;
    fn insert(tx: &Self::Tx<'_>, record: &Record<F, H>) -> Result<Inserted, Self::Error>;
    fn delete(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error>;
    fn get_in(tx: &Self::Tx<'_>, hash: &H::Output) -> Result<Option<F>, Self::Error>;
    fn get(&self, hash: &H::Output) -> Result<Option<F>, Self::Error>;
    fn list(&self) -> Result<Vec<(H::Output, F)>, Self::Error>;
}
```

`write` runs `f` in one transaction. It commits when `f` returns `Ok`, and it rolls back otherwise. The closure form lets a catalog hold its connection behind a mutex. A transaction that borrows from a mutex guard can't be returned from a method.

`insert` reports `Inserted::New` or `Inserted::AlreadyPresent`. A second record for the same hash is not an error.

`Catalog` is also implemented for `Arc<C>`. So an app can share one database handle between the store and its own code.

The crate ships two ready catalogs. Each one comes with free functions that an app can call to implement `Catalog` on its own database type.

- `sqlite::Sqlite`, behind the `catalog-sqlite` feature, wraps a `rusqlite::Connection` in a mutex. It uses a `blob` table with the columns `hash`, `format`, `size_bytes`, and `added_at`. An app that creates the table itself can add stricter checks, such as the exact hash length. The `catalog-sqlite` feature does not pick a SQLite library: the app enables `bundled` or links its own.
- `redb::Redb`, behind the `catalog-redb` feature, wraps a `redb::Database`. It uses a `blobs` table keyed by the digest bytes.

Both store `added_at` as nanoseconds since the Unix epoch, in an `i64`.

### Store

```rust
pub struct BlobStore<C, F, H = Blake3, const N: usize = 2>;

impl<C, F> BlobStore<C, F> {
    pub fn open(root: impl Into<PathBuf>, catalog: C) -> Result<Self, Error>;
}

impl<C: Catalog<F, H>, F: Format, H: Hasher, const N: usize> BlobStore<C, F, H, N> {
    pub fn open_with(root: impl Into<PathBuf>, catalog: C) -> Result<Self, Error>;
    pub fn root(&self) -> &Path;
    pub fn catalog(&self) -> &C;
    pub fn begin(&self) -> Result<Ingest<F, H>, Error>;
    pub fn save(&self, bytes: &[u8]) -> Result<Staged<F, H>, Error>;
    pub fn land(&self, staged: Staged<F, H>) -> Result<Landed<F, H>, Error>;
    pub fn write<T, E: From<Error>>(
        &self,
        f: impl FnOnce(&mut BlobTx<'_, '_, C, F, H>) -> Result<T, E>,
    ) -> Result<Written<T, H::Output>, E>;
    pub fn path_of(&self, hash: &H::Output, format: F) -> PathBuf;
    pub fn get(&self, hash: &H::Output) -> Result<Option<F>, Error>;
    pub fn list(&self) -> Result<Vec<(H::Output, F)>, Error>;
    pub fn sweep(&self) -> Result<SweepReport, Error>;
    pub fn missing(&self) -> Result<Vec<(H::Output, F)>, Error>;
}
```

`get` and `list` read the catalog through the store. A catalog type can serve several hashers, so calling them on the catalog directly can need a type annotation.

`open` uses the default hasher and depth, and needs the `hasher-blake3` feature. Without it, `H` has no default, and `open_with` is the only constructor. Default type parameters do not take part in type inference, so the defaults sit on a separate impl block. `open_with` takes any hasher and depth through a turbofish. A compile-time assertion checks that `N <= H::LEN`.

The other types:

- `Ingest` streams bytes into `tmp/` and hashes them as they arrive. `finish` runs fsync and returns a `Staged`. A failed `write` can leave part of a chunk in the file that the hash never saw. So after one, every later `write` and `finish` fails with `Error::IngestFailed`, and a retry needs a new `Ingest`.
- `Staged` exposes the hash, the size in bytes, and the sniffed format. Dropping it deletes the temp file.
- `Landed` is the proof that a file is in place. Only `land` creates one. Dropping it without recording it leaves an orphan file, which `sweep` removes.
- `BlobTx` wraps the catalog transaction. `insert(landed, added_at)` records a blob. `remove(hash)` deletes a record now, and the file after the commit. `inner()` gives the app the raw transaction for its own rows.
- `Written` holds the closure's value and the list of file deletions that failed. Each failure is a `FailedRemoval`, with the hash, the path, and the error.

## Flows

### Save

1. `begin` creates a temp file in `tmp/` and registers it as live.
2. `Ingest::write` streams the bytes and hashes them. `finish` runs fsync on the temp file and sniffs the format.
3. The app checks for a duplicate with `Staged::hash`, if it wants to.
4. `land`, under the file lock:
   1. It marks the hash in flight.
   2. The first time this process lands a file in a shard directory, it creates the directory, and runs fsync on every directory from its parent up to the root. A directory that exists is not proof: an earlier fsync of it may have failed. So the store keeps a set of the shard directories it has made durable, and adds one only after every fsync succeeded.
   3. It renames the temp file into its shard directory.
   4. It runs fsync on the shard directory.
5. `write` opens a catalog transaction. The closure calls `BlobTx::insert` and writes the app's rows.
6. After the commit, or after a rollback, the store clears the in-flight mark.

### Delete

1. Inside `write`, `BlobTx::remove` deletes the record and queues the file.
2. After the commit, the store takes the file lock. It skips a queued file if its hash is in flight, or if the catalog has a record for it again. Otherwise it deletes the file.
3. Each failure goes into `Written::failed_removals`. The file stays until the next `sweep`.

### Sweep

`sweep` is manual, and it is safe to run at any time. For each entry, under the file lock:

- It deletes a temp file that is not live.
- It deletes a file with a well-formed name in the right directory, when its hash is not in flight and the catalog has no record of that hash with that format.
- It keeps everything else, and lists it in `unrecognized`.
- It lists each failed deletion in `failed`, and it continues past it.

## Locks

The store has two locks:

- The file lock covers every step that adds a file to the tree or takes one away: `begin`, `land`, the deletions after a commit, and `sweep`.
- The state lock covers the set of hashes in flight and the set of live temp files. Nothing else is locked while it is held.

The order is always: the file lock, then the catalog, then the state lock. Dropping a `Landed` or a `Staged` takes only the state lock. So an app can drop either one inside a `write` closure without a deadlock.

An app must not call `land` inside a `write` closure. That would take the file lock while the catalog is held.

## Properties

| # | Property | How it holds |
|---|---|---|
| P1 | If the catalog has a record, the file exists at `path_of`. | `insert` accepts only a `Landed`. A file is deleted only after its record is gone, under the file lock. |
| P2 | A failed save writes no record. | A failed `land` gives no `Landed`. A failed closure or commit rolls back the transaction. |
| P3 | The blob record and the app's rows commit atomically. | Both are written in one catalog transaction. |
| P4 | A file's name always matches its content. | Files enter the tree only by a rename of a temp file that was fully hashed and fsynced. |
| P5 | A crash never breaks P1. | The file, and every directory above it up to the root, is durable before its record commits. `open` makes the root and any ancestors it creates durable. A crash leaves at most an orphan file. |
| P6 | A file without a record is temporary. | Orphans come only from a crash, a rollback, a dropped `Landed`, or a failed deletion. `sweep` removes them. |
| P7 | Saves, deletes, and sweeps of the same hash do not race. | All three touch files under the file lock. The in-flight mark covers the time between `land` and the commit. |
| P8 | A store opened with the wrong settings fails at once. | `layout.toml` records the hasher and `N`, and `open` checks both. |

## Limits

- **Damage from outside the store.** A file can still disappear through manual deletion or a bad disk. `missing` detects this.
- **Raw writes to the blob table.** The app can write SQL through `BlobTx::inner`. A record inserted by hand can break P1. A record deleted by hand only leaves an orphan.
- **One store per catalog.** The lock file stops a second process on the same root. It can't detect two roots that share one catalog.
- **No promise that every file has a record at every moment.** That would need one atomic step across the file system and the database.

## Choices

### Constructor, not builder or typestate

A builder can't set a const generic from a runtime value. The hasher has no state. With defaults for the hasher and `N`, only the root and the catalog are required. A constructor with those two arguments catches a missing input at compile time, like a typestate does.

### Sync, not async

- Tokio has no native async file I/O on Linux or macOS. `tokio::fs` runs each call on a blocking thread.
- Hashing is CPU work.
- The SQLite and redb bindings are sync.
- A sync crate works with any async runtime.
- An app calls `land` and `write` from an async task, or through `spawn_blocking` if fsync latency matters.

### An own `Hasher` trait

The standard library's `std::hash::Hasher` serves hash tables. Its `finish` returns a `u64`, and its hashers do not resist collisions. A store needs a digest of at least 32 bytes that resists collisions, so the trait does not fit.

The RustCrypto `digest::Digest` trait comes closer. `sha2` implements it, but `blake3` implements it only behind an unstable feature. Neither trait gives a name for the layout file, or an output with hex display and parsing. So the crate defines a small trait of its own.

### The store owns the catalog

The store must control the commit, to delete a file only after its record is gone. The app still writes its own rows in the same transaction, through `BlobTx::inner`.

## Features

| Feature | Adds |
|---|---|
| `format-presets` | The nine preset formats, `identify`, and the `formats!` macro, on `infer`. |
| `hasher-blake3` | The `Blake3` hasher, and the default type parameter `H = Blake3`. |
| `hasher-sha256` | The `Sha256` hasher. |
| `catalog-sqlite` | The `sqlite` module, on `rusqlite`. |
| `catalog-redb` | The `redb` module, on `redb`. |
| `serde` | `Serialize` and `Deserialize` for `Digest`, as a hex string. |

No feature is on by default.

## Development

Every tool comes from the nix devShell:

```sh
just fmt
just lint
just test
```

## License

Copyright 2026 larry0x.

Licensed under the [Apache License, Version 2.0](LICENSE).
