// NOTE that this example does not produce correct (expected) results. Open
// an issue or create a PR if you have any idea why or how to solve it.
#![allow(non_snake_case)]

use ::humantime::format_rfc3339_seconds;
use ::pbr::ProgressBar;
use ::rand::prelude::*;
use ::rayon::prelude::*;

use std::{fs::OpenOptions, io::prelude::*, time::SystemTime};

use ising_lib::prelude::*;

const DIR_PATH: &str = "results";

struct Params {
    T_range: (f64, f64),
    flips_to_skip: usize,
    measurements_per_T: usize,
    flips_per_measurement: usize,
    attempts_per_flip: usize,
    lattice_size: usize,
    J: f64,
    K: f64,
}

struct Record {
    T: f64,
    dE: f64,
    I: f64,
    X: f64,
}

fn main() {
    let size = 50;
    let params = Params {
        // the phase transition occurs at ~2.29
        T_range: (0.2, 4.0),
        // allow the spin lattice to "cool down"
        flips_to_skip: 300_000,
        // the more measurements taken at each T, the more precise the results
        // will be
        measurements_per_T: 2_000,
        // just a rule of thumb
        flips_per_measurement: size * size,
        attempts_per_flip: 20,
        lattice_size: size,
        J: 1.0,
        K: 1.0,
    };

    let Ts = TRange::new_step(params.T_range.0, params.T_range.1, 0.1)
        .collect::<Vec<f64>>();

    let bar_count = (params.measurements_per_T * Ts.len()) as u64;

    let mut pb = ProgressBar::new(bar_count);
    pb.set_width(Some(80));
    pb.show_message = true;
    pb.message("Running...");

    let (pb_tx, pb_rx) = std::sync::mpsc::channel();
    let pb_txs = (0..Ts.len()).map(|_| pb_tx.clone()).collect::<Vec<_>>();

    let handle = std::thread::spawn(move || {
        for _ in 0..bar_count {
            let _ = pb_rx.recv();
            pb.inc();
        }

        pb.finish_print("Finished!");
    });

    // run simulation at different temperatures in parallel
    let mut records: Vec<Record> = Ts
        .into_iter()
        .zip(pb_txs)
        .collect::<Vec<_>>()
        .into_par_iter() // notice the `into_par_iter`
        .map(|(T, pb_tx)| {
            let mut rng = thread_rng();
            let mut lattice = Lattice::new(params.lattice_size);

            // "cool" the lattice to its natural state at temperature `T`
            (0..params.flips_to_skip).for_each(|_| {
                let _ = (0..params.attempts_per_flip)
                    .map(|_| {
                        let ix = lattice.gen_random_index();
                        let E_diff = lattice.measure_E_diff(ix, params.J);
                        let probability =
                            calc_flip_probability(E_diff, T, params.K);

                        if probability > rng.gen() {
                            lattice.flip_spin(ix);

                            true
                        } else {
                            false
                        }
                    })
                    .take_while(|already_flipped| !already_flipped)
                    .count();
            });

            // run the actual simulation
            let (Es, Is) = (0..params.measurements_per_T)
                .map(|_| {
                    (0..params.flips_per_measurement).for_each(|_| {
                        let _ = (0..params.attempts_per_flip)
                            .map(|_| {
                                let ix = lattice.gen_random_index();
                                let E_diff =
                                    lattice.measure_E_diff(ix, params.J);
                                let probability =
                                    calc_flip_probability(E_diff, T, params.K);

                                if probability > rng.gen() {
                                    lattice.flip_spin(ix);

                                    true // the flip has already occured
                                } else {
                                    false // the flip has not occured yet
                                }
                            })
                            .take_while(|already_flipped| !already_flipped)
                            .count();
                    });

                    let _ = pb_tx.send(());

                    (lattice.measure_E(params.J), lattice.measure_I())
                })
                .unzip::<_, _, Vec<_>, Vec<_>>();

            let dE = calc_dE(&Es, T);
            let I = Is.iter().sum::<f64>() / Is.len() as f64;
            let X = calc_X(&Es);

            Record { T, dE, I, X }
        })
        .collect();

    records.sort_by(|a, b| {
        a.T.partial_cmp(&b.T).unwrap_or(std::cmp::Ordering::Less)
    });

    let contents = {
        let headers = format!("{:>5}{:>30}{:>15}{:>20}", "T", "dE", "I", "X");
        let results = records
            .iter()
            .map(|r| {
                format!("{:>5.2}{:>30.5}{:>15.5}{:>20.10}", r.T, r.dE, r.I, r.X)
            })
            .collect::<Vec<String>>()
            .join("\n");
        let mut contents = vec![headers, results].join("\n");
        contents.push_str("\n");

        contents
    };

    let _ = handle.join();

    let path = {
        format!(
            "{}/results-parallel-{}.txt",
            DIR_PATH,
            format_rfc3339_seconds(SystemTime::now())
        )
    };

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(path)
        .unwrap();
    file.write_all(contents.as_bytes()).unwrap();
}