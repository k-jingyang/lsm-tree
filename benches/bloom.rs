use criterion::{criterion_group, criterion_main, Criterion};

fn filter_construction(c: &mut Criterion) {
    use lsm_tree::segment::filter::standard_bloom::Builder;

    let mut filter = Builder::with_fp_rate(500_000_000, 0.01);

    c.bench_function("bloom filter add key", |b| {
        b.iter(|| {
            let key = nanoid::nanoid!();
            filter.set_with_hash(Builder::get_hash(key.as_bytes()));
        });
    });
}

fn filter_contains(c: &mut Criterion) {
    use lsm_tree::segment::filter::standard_bloom::Builder;

    let keys = (0..100_000u128)
        .map(|x| x.to_be_bytes().to_vec())
        .collect::<Vec<_>>();

    for fpr in [0.01, 0.001, 0.0001, 0.00001] {
        let mut filter = Builder::with_fp_rate(100_000_000, fpr);

        for key in &keys {
            filter.set_with_hash(Builder::get_hash(key));
        }

        let mut rng = rand::rng();

        let filter = filter.build();

        c.bench_function(
            &format!(
                "standard bloom filter contains key, true positive ({}%)",
                fpr * 100.0,
            ),
            |b| {
                b.iter(|| {
                    use rand::seq::IndexedRandom;

                    let sample = keys.choose(&mut rng).unwrap();
                    let hash = Builder::get_hash(sample);
                    assert!(filter.contains_hash(hash));
                });
            },
        );
    }
}

criterion_group!(benches, filter_construction, filter_contains,);
criterion_main!(benches);
