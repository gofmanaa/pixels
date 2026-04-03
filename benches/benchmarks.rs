use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use pixels::filters::Filter;
use pixels::render::{YuvLut, sample_bilinear};
use std::hint::black_box;
fn bench_filters(c: &mut Criterion) {
    let mut group = c.benchmark_group("filters");
    let px = 0x00AABBCC;
    let row = 10;

    let filters = [
        Filter::Normal,
        Filter::Grayscale,
        Filter::Invert,
        Filter::Sepia,
        Filter::RedBoost,
        Filter::CoolBlue,
        Filter::Threshold,
        Filter::Scanlines,
        Filter::Vaporwave,
        Filter::Noir,
    ];

    for filter in filters {
        group.bench_with_input(
            BenchmarkId::from_parameter(filter.name()),
            &filter,
            |b, f| b.iter(|| f.apply(black_box(px), black_box(row))),
        );
    }
    group.finish();
}

fn bench_yuv_lookup(c: &mut Criterion) {
    let lut = YuvLut::build();
    c.bench_function("yuv_lookup", |b| {
        b.iter(|| lut.lookup(black_box(128), black_box(128), black_box(128)))
    });
}

fn bench_sample_bilinear(c: &mut Criterion) {
    let width = 640;
    let height = 480;
    let frame = vec![0u32; width * height];

    c.bench_function("sample_bilinear", |b| {
        b.iter(|| {
            sample_bilinear(
                black_box(&frame),
                width,
                height,
                black_box(320.5),
                black_box(240.5),
            )
        })
    });
}

criterion_group!(
    benches,
    bench_filters,
    bench_yuv_lookup,
    bench_sample_bilinear
);
criterion_main!(benches);
