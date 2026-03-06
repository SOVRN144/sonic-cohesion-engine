use crate::{ci_check, graph, plugin_chain, ref_canon, translation_matrix};
use anyhow::{anyhow, Context};
use serde::Serialize;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: &str = "1.0";

pub trait Operator: Sync {
    fn name(&self) -> &'static str;
    fn output_path_template(&self) -> &'static str;
    fn requires_run_id(&self) -> bool;
    fn supports_last_n(&self) -> bool;
    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult>;
}

#[derive(Debug, Clone, Copy)]
pub struct OperatorRunRequest<'a> {
    pub project_root: &'a Path,
    pub run_id: Option<&'a str>,
    pub last_n: usize,
}

#[derive(Debug, Clone)]
pub struct OperatorRunResult {
    pub output_path: PathBuf,
    pub output: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OperatorListEntry {
    pub name: String,
    pub output_path: String,
    pub requires_run_id: bool,
    pub supports_last_n: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OperatorListOutput {
    pub schema_version: String,
    pub operators: Vec<OperatorListEntry>,
}

struct RefCanonOperator;
struct MixopsCiOperator;
struct TranslationMatrixOperator;
struct StemgraphOperator;
struct PluginChainCompilerOperator;

static REF_CANON_OPERATOR: RefCanonOperator = RefCanonOperator;
static MIXOPS_CI_OPERATOR: MixopsCiOperator = MixopsCiOperator;
static TRANSLATION_MATRIX_OPERATOR: TranslationMatrixOperator = TranslationMatrixOperator;
static STEMGRAPH_OPERATOR: StemgraphOperator = StemgraphOperator;
static PLUGIN_CHAIN_COMPILER_OPERATOR: PluginChainCompilerOperator = PluginChainCompilerOperator;

// Declaration order = display order in operators list
static OPERATORS: [&dyn Operator; 5] = [
    &REF_CANON_OPERATOR,
    &MIXOPS_CI_OPERATOR,
    &TRANSLATION_MATRIX_OPERATOR,
    &STEMGRAPH_OPERATOR,
    &PLUGIN_CHAIN_COMPILER_OPERATOR,
];

impl Operator for RefCanonOperator {
    fn name(&self) -> &'static str {
        "ref_canon"
    }

    fn output_path_template(&self) -> &'static str {
        "SCE/operators/ref_canon/latest.json"
    }

    fn requires_run_id(&self) -> bool {
        false
    }

    fn supports_last_n(&self) -> bool {
        false
    }

    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult> {
        let (output, output_path) = ref_canon::write_operator_output(request.project_root)?;
        build_result(output_path, &output)
    }
}

impl Operator for MixopsCiOperator {
    fn name(&self) -> &'static str {
        "mixops_ci"
    }

    fn output_path_template(&self) -> &'static str {
        "SCE/operators/mixops_ci/latest.json"
    }

    fn requires_run_id(&self) -> bool {
        false
    }

    fn supports_last_n(&self) -> bool {
        true
    }

    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult> {
        let (output, output_path) =
            ci_check::write_operator_output(request.project_root, request.run_id, request.last_n)?;
        build_result(output_path, &output)
    }
}

impl Operator for TranslationMatrixOperator {
    fn name(&self) -> &'static str {
        "translation_matrix"
    }

    fn output_path_template(&self) -> &'static str {
        "SCE/operators/translation_matrix/<run_id>.json"
    }

    fn requires_run_id(&self) -> bool {
        true
    }

    fn supports_last_n(&self) -> bool {
        false
    }

    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult> {
        let run_id = request
            .run_id
            .ok_or_else(|| anyhow!("translation_matrix requires --run-id"))?;
        let (output, output_path) = translation_matrix::run(request.project_root, run_id)?;
        build_result(output_path, &output)
    }
}

impl Operator for StemgraphOperator {
    fn name(&self) -> &'static str {
        "stemgraph"
    }

    fn output_path_template(&self) -> &'static str {
        "SCE/operators/stemgraph/latest.json"
    }

    fn requires_run_id(&self) -> bool {
        false
    }

    fn supports_last_n(&self) -> bool {
        false
    }

    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult> {
        let (output, output_path) = graph::write_operator_output(request.project_root)?;
        build_result(output_path, &output)
    }
}

impl Operator for PluginChainCompilerOperator {
    fn name(&self) -> &'static str {
        "plugin_chain_compiler"
    }

    fn output_path_template(&self) -> &'static str {
        "SCE/operators/plugin_chain_compiler/<run_id>.json"
    }

    fn requires_run_id(&self) -> bool {
        true
    }

    fn supports_last_n(&self) -> bool {
        false
    }

    fn run(&self, request: &OperatorRunRequest<'_>) -> anyhow::Result<OperatorRunResult> {
        let run_id = request
            .run_id
            .ok_or_else(|| anyhow!("plugin_chain_compiler requires --run-id"))?;
        let (output, output_path) =
            plugin_chain::write_operator_output(request.project_root, run_id)?;
        build_result(output_path, &output)
    }
}

pub fn list() -> OperatorListOutput {
    OperatorListOutput {
        schema_version: SCHEMA_VERSION.to_string(),
        operators: OPERATORS
            .iter()
            .map(|operator| OperatorListEntry {
                name: operator.name().to_string(),
                output_path: operator.output_path_template().to_string(),
                requires_run_id: operator.requires_run_id(),
                supports_last_n: operator.supports_last_n(),
            })
            .collect(),
    }
}

pub fn run(request: OperatorRunRequest<'_>, name: &str) -> anyhow::Result<OperatorRunResult> {
    let operator = OPERATORS
        .iter()
        .find(|operator| operator.name() == name)
        .ok_or_else(|| anyhow!("unknown operator: {name}"))?;
    operator.run(&request)
}

fn build_result<T: Serialize>(
    output_path: PathBuf,
    output: &T,
) -> anyhow::Result<OperatorRunResult> {
    Ok(OperatorRunResult {
        output_path,
        output: serde_json::to_value(output).context("serialize operator output")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{list, run, OperatorRunRequest};
    use crate::{graph, plugin_chain};
    use std::path::Path;

    #[test]
    fn list_schema_and_order_are_locked() {
        let output = list();
        assert_eq!(output.schema_version, "1.0");
        let names = output
            .operators
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "ref_canon",
                "mixops_ci",
                "translation_matrix",
                "stemgraph",
                "plugin_chain_compiler"
            ]
        );
    }

    #[test]
    fn run_arg_semantics_and_fixed_paths_are_locked() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let source_path = td.path().join("audio/mix.wav");
        let run_dir = project_root.join("SCE/assets/asset-a/analysis/run-1");
        std::fs::create_dir_all(source_path.parent().expect("audio parent")).expect("mkdir");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(&source_path, b"wave").expect("write source");
        plugin_chain::write_worker_chain_meta(&run_dir, "run-1", &source_path)
            .expect("write chain meta");
        graph::link(&project_root, "stem-a", "mix-a").expect("link graph");

        let translation_err = run(
            OperatorRunRequest {
                project_root: &project_root,
                run_id: None,
                last_n: 25,
            },
            "translation_matrix",
        )
        .expect_err("missing run id");
        assert!(translation_err.to_string().contains("requires --run-id"));

        let plugin_err = run(
            OperatorRunRequest {
                project_root: &project_root,
                run_id: None,
                last_n: 25,
            },
            "plugin_chain_compiler",
        )
        .expect_err("missing run id");
        assert!(plugin_err.to_string().contains("requires --run-id"));

        let stemgraph = run(
            OperatorRunRequest {
                project_root: &project_root,
                run_id: Some("ignored"),
                last_n: 1,
            },
            "stemgraph",
        )
        .expect("stemgraph");
        let canonical_root =
            crate::policy_registry::canonical_project_root(&project_root).expect("canonical root");
        assert_eq!(
            stemgraph.output_path,
            crate::paths::stemgraph_operator_output_path(&canonical_root)
        );
    }

    #[test]
    fn operator_outputs_are_byte_stable_without_timestamps() {
        let td = tempfile::tempdir().expect("tempdir");
        let project_root = td.path().join("project");
        let ref_path = td.path().join("ref.wav");
        let source_path = td.path().join("audio/mix.wav");
        let run_dir = project_root.join("SCE/assets/asset-a/analysis/run-1");

        write_wav_i16(&ref_path).expect("write ref");
        std::fs::create_dir_all(source_path.parent().expect("audio parent")).expect("mkdir");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(&source_path, b"wave").expect("write source");
        crate::ref_canon::add_ref(&project_root, &ref_path, Some("Alpha"), "warm", false)
            .expect("add ref");
        plugin_chain::write_worker_chain_meta(&run_dir, "run-1", &source_path)
            .expect("write chain meta");

        let first = run(
            OperatorRunRequest {
                project_root: &project_root,
                run_id: Some("run-1"),
                last_n: 25,
            },
            "plugin_chain_compiler",
        )
        .expect("first run");
        let first_bytes = std::fs::read(&first.output_path).expect("read first");

        let second = run(
            OperatorRunRequest {
                project_root: &project_root,
                run_id: Some("run-1"),
                last_n: 25,
            },
            "plugin_chain_compiler",
        )
        .expect("second run");
        let second_bytes = std::fs::read(&second.output_path).expect("read second");

        assert_eq!(first_bytes, second_bytes);
        let text = String::from_utf8(first_bytes).expect("utf8");
        assert!(!text.contains("finished_at"));
        assert!(!text.contains("generated_at"));
    }

    fn write_wav_i16(path: &Path) -> anyhow::Result<()> {
        let sample_rate_hz = 48_000u32;
        let channels = 1u16;
        let frames = 512usize;
        let mut pcm = Vec::<i16>::with_capacity(frames * channels as usize);
        for frame_idx in 0..frames {
            let value = ((frame_idx as f64 / 16.0).sin() * 0.25 * 32767.0).round() as i16;
            pcm.push(value);
        }

        let data_size = (pcm.len() * 2) as u32;
        let byte_rate = sample_rate_hz * channels as u32 * 2;
        let block_align = channels * 2;
        let riff_size = 36 + data_size;

        let mut file = std::fs::File::create(path)?;
        std::io::Write::write_all(&mut file, b"RIFF")?;
        std::io::Write::write_all(&mut file, &riff_size.to_le_bytes())?;
        std::io::Write::write_all(&mut file, b"WAVE")?;
        std::io::Write::write_all(&mut file, b"fmt ")?;
        std::io::Write::write_all(&mut file, &16u32.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &1u16.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &channels.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &sample_rate_hz.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &byte_rate.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &block_align.to_le_bytes())?;
        std::io::Write::write_all(&mut file, &16u16.to_le_bytes())?;
        std::io::Write::write_all(&mut file, b"data")?;
        std::io::Write::write_all(&mut file, &data_size.to_le_bytes())?;
        for sample in pcm {
            std::io::Write::write_all(&mut file, &sample.to_le_bytes())?;
        }
        Ok(())
    }
}
