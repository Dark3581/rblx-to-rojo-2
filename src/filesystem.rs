use crate::structures::*;
use log::error;
use serde::{ser::SerializeMap, Serialize, Serializer};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

const SRC: &str = "src";

fn serialize_project_tree<S: Serializer>(
    tree: &BTreeMap<String, TreePartition>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(tree.len() + 1))?;
    map.serialize_entry("$className", "DataModel")?;
    for (k, v) in tree {
        map.serialize_entry(k, v)?;
    }
    map.end()
}

#[derive(Clone, Debug, Serialize)]
struct Project {
    name: String,
    #[serde(serialize_with = "serialize_project_tree")]
    tree: BTreeMap<String, TreePartition>,
}

impl Project {
    fn new() -> Self {
        Self {
            name: "project".to_string(),
            tree: BTreeMap::new(),
        }
    }
}

/// Paths in instructions are relative to src/, but the project file is one level up.
fn prefix_paths(partition: &mut TreePartition) {
    if let Some(path) = &partition.path {
        partition.path = Some(Path::new(SRC).join(path));
    }

    for child in partition.children.values_mut() {
        prefix_paths(child);
    }
}

#[derive(Clone, Debug)]
pub struct FileSystem {
    project: Project,
    root: PathBuf,
    source: PathBuf,
    errors: Vec<String>,
}

impl FileSystem {
    pub fn from_root(root: PathBuf) -> Self {
        let source = root.join(SRC);
        let mut errors = Vec::new();

        if let Err(error) = fs::create_dir_all(&source) {
            errors.push(format!("can't create {}: {}", source.display(), error));
        }

        Self {
            project: Project::new(),
            root,
            source,
            errors,
        }
    }

    /// Everything that failed to write. Processing carries on after a failure
    /// so one bad file doesn't stop the rest of the export.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    fn fail(&mut self, message: String) {
        error!("{}", message);
        self.errors.push(message);
    }
}

impl InstructionReader for FileSystem {
    fn read_instruction<'a>(&mut self, instruction: Instruction<'a>) {
        match instruction {
            Instruction::AddToTree {
                name,
                mut partition,
            } => {
                if self.project.tree.contains_key(&name) {
                    self.fail(format!(
                        "two services are both named {:?}; only the first is in default.project.json",
                        name
                    ));
                    return;
                }

                prefix_paths(&mut partition);
                self.project.tree.insert(name, partition);
            }

            Instruction::CreateFile { filename, contents } => {
                let path = self.source.join(&filename);
                if let Err(error) = File::create(&path).and_then(|mut file| file.write_all(&contents)) {
                    self.fail(format!("can't write {}: {}", path.display(), error));
                }
            }

            Instruction::CreateFolder { folder } => {
                let path = self.source.join(&folder);
                if let Err(error) = fs::create_dir_all(&path) {
                    self.fail(format!("can't create folder {}: {}", path.display(), error));
                }
            }
        }
    }

    fn finish_instructions(&mut self) {
        let path = self.root.join("default.project.json");
        let contents = serde_json::to_string_pretty(&self.project).expect("couldn't serialize project");

        if let Err(error) = fs::write(&path, contents) {
            self.fail(format!("can't write {}: {}", path.display(), error));
        }
    }
}
