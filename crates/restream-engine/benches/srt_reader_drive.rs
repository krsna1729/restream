//! SRT direct-play reader drive on the ingress Owner: each burst of shared
//! muxer TS chunks is pulled by every attached reader, sliced into SRT-sized
//! fragments and handed to the send path in bounded per-visit runs
//! (`OwnerLoop::drive_readers`), until every reader is drained. The socket
//! send itself is out of scope (`restream_engine::srt::bench_entry`).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_engine::srt::bench_entry::ReaderDrive;

fn bench_srt_reader_drive(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut group = c.benchmark_group("srt_reader_drive");
    for readers in [1, 8] {
        let mut drive = rt.block_on(ReaderDrive::start(readers));
        group.throughput(Throughput::Bytes(drive.delivered_bytes() as u64));
        group.bench_function(format!("{readers}_readers"), |b| {
            b.iter_custom(|iterations| drive.run(iterations));
        });
        // The readers' muxer leases release on the runtime.
        rt.block_on(async move { drop(drive) });
    }
    group.finish();
}

criterion_group!(benches, bench_srt_reader_drive);
criterion_main!(benches);
