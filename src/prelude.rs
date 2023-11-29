pub use anyhow::bail;
pub use anyhow::anyhow;

pub type Void = anyhow::Result<()>;
pub type Res<T> = anyhow::Result<T>;