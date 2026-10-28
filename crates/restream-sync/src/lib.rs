//! Zero-copy synchronization, ring buffers, and bounded queues for Restream.

pub mod byte_queue;
pub mod frame_units;
pub mod lock;
pub mod reader;
pub mod ring_buffer;
pub mod standby_gop;
pub mod subscribers;

pub use byte_queue::{DEFAULT_RECORDING_QUEUE_CAPACITY, MemoryQueue};
pub use frame_units::VideoConfigFollower;
pub use lock::{lock, read, write};
pub use reader::{Reader, ReaderInfo, ReaderSnapshot};
pub use ring_buffer::{
    AlignedAtomicUsize, BURST_HIST_BUCKETS, DEFAULT_RING_CAPACITY,
    DEFAULT_TRANSCODER_RING_CAPACITY, DtsEnforcer, MEDIA_PRODUCER_BATCH_PACKETS,
    MEDIA_PULL_BURST_PACKETS, PayloadStats, RingBuffer, RingSlot,
};
pub use standby_gop::StandbyGopCache;
pub use subscribers::{PublishSubscribers, PublishWake};
