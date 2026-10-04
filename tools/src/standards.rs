use crate::common::*;
use serde_json::{json, Value};
use std::path::Path;

fn signal(
    input: &Path,
    rate: u32,
    sections: &[(f64, f64)],
    frequency: f64,
    phase: f64,
    fade: bool,
    amplitude: bool,
) -> Result<()> {
    let total: usize = sections
        .iter()
        .map(|(s, _)| (s * rate as f64).round() as usize)
        .sum();
    let mut samples = Vec::with_capacity(total * 2);
    let mut cursor = 0;
    for &(duration, level) in sections {
        let count = (duration * rate as f64).round() as usize;
        let scale = if amplitude {
            level
        } else {
            10.0f64.powf(level / 20.0)
        };
        for i in 0..count {
            let gain = if fade {
                1.0f64
                    .min(i as f64 / (rate as f64 * 0.01))
                    .min((count - 1 - i) as f64 / (rate as f64 * 0.01))
            } else {
                1.0
            };
            let value = (scale
                * gain
                * (std::f64::consts::TAU * frequency * (cursor + i) as f64 / rate as f64 + phase)
                    .sin()
                * (1u32 << 23) as f64)
                .round() as i32;
            check(
                (-(1 << 23)..(1 << 23)).contains(&value),
                "standards fixture would clip",
            )?;
            samples.extend_from_slice(&[value, value]);
        }
        cursor += count;
    }
    write_wave(input, rate, 24, 2, &samples)
}
pub fn execute(options: &Options) -> Result<Value> {
    let temp = Temp::new("standards")?;
    let mut rows = Vec::new();
    let cases: Vec<Vec<(f64, f64)>> = vec![
        vec![(20.0, -23.0)],
        vec![(20.0, -33.0)],
        vec![(10.0, -36.0), (60.0, -23.0), (10.0, -36.0)],
        vec![
            (10.0, -72.0),
            (10.0, -36.0),
            (60.0, -23.0),
            (10.0, -36.0),
            (10.0, -72.0),
        ],
        vec![(20.0, -26.0), (20.1, -20.0), (20.0, -26.0)],
    ];
    for (index, sections) in cases.iter().enumerate() {
        let case = index + 1;
        let input = temp.0.join(format!("case{case}.wav"));
        signal(&input, 48000, sections, 1000.0, 0.0, false, false)?;
        let report = native(&options.binary, "analyze", &[&input], &[])?;
        let actual = report["integrated_lufs"].as_f64().ok_or("standards LUFS")?;
        let expected = if case == 2 { -33.0 } else { -23.0 };
        check(
            (actual - expected).abs() <= 0.1,
            "EBU synthesized integrated tolerance",
        )?;
        rows.push(json!({"test":case,"metric":"integrated_lufs","expected":expected,"actual":actual,"tolerance":0.1,"passed":true}));
    }
    for rate in [44100, 48000, 96000] {
        for (case, divisor, phase, amplitude) in [
            (15, 4.0, 0.0, 0.5),
            (16, 4.0, std::f64::consts::FRAC_PI_4, 0.5),
            (17, 6.0, std::f64::consts::PI / 3.0, 0.5),
            (18, 8.0, 3.0 * std::f64::consts::PI / 8.0, 0.5),
            (19, 4.0, std::f64::consts::FRAC_PI_4, 1.41),
        ] {
            let input = temp.0.join(format!("case{case}-{rate}.wav"));
            signal(
                &input,
                rate,
                &[(2.0, amplitude)],
                rate as f64 / divisor,
                phase,
                true,
                true,
            )?;
            let report = native(&options.binary, "analyze", &[&input], &[])?;
            let actual = report["true_peak_dbtp"]
                .as_f64()
                .ok_or("standards true peak")?;
            let expected = if case == 19 { 3.0 } else { -6.0 };
            check(
                (expected - 0.4..=expected + 0.2).contains(&actual),
                "EBU synthesized true peak tolerance",
            )?;
            rows.push(json!({"test":case,"sample_rate":rate,"metric":"true_peak_dbtp","expected":expected,"actual":actual,"tolerance_lower":-0.4,"tolerance_upper":0.2,"passed":true}));
        }
    }
    Ok(
        json!({"harness":"audeniq-qc-tools Rust","reference":"EBU Tech 3341 (November 2023), Table 1","source":"https://tech.ebu.ch/files/live/sites/tech/files/shared/tech/tech3341.pdf","test_material":"Independently synthesized from published descriptions; not the original EBU ZIP.","scope":"Integrated cases 1..5 and true peak 15..19 at three rates. Cases 6..14 and transient cases 20..23 are not covered.","certified":false,"status":"passed","results":rows}),
    )
}
