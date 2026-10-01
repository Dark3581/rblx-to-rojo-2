use rbx_dom_weak::types::Variant;
use serde::{Deserialize, Serialize, Serializer};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

// Windows issues!
fn replace_backslashes<S: Serializer>(
    path: &Option<PathBuf>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match path {
        Some(value) => value
            .to_string_lossy()
            .replace("\\", "/")
            .serialize(serializer),

        None => serializer.serialize_none(),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TreePartition {
    // Left out for nodes whose $path points at a file, where Rojo takes the
    // class from the file and rejects a $className.
    #[serde(rename = "$className")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,

    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub children: BTreeMap<String, TreePartition>,

    #[serde(rename = "$ignoreUnknownInstances")]
    pub ignore_unknown_instances: bool,

    #[serde(rename = "$path")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "replace_backslashes")]
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct MetaFile {
    #[serde(rename = "className")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,

    #[serde(rename = "ignoreUnknownInstances")]
    pub ignore_unknown_instances: bool,

    // Fully qualified values ({"Bool": false}, {"Enum": 2}, ...), which Rojo
    // reads without needing to know the property's type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Variant>,
}

#[derive(Clone, Debug)]
pub enum Instruction<'a> {
    AddToTree {
        name: String,
        partition: TreePartition,
    },

    CreateFile {
        filename: Cow<'a, Path>,
        contents: Cow<'a, [u8]>,
    },

    CreateFolder {
        folder: Cow<'a, Path>,
    },
}

pub trait InstructionReader {
    fn finish_instructions(&mut self) {}
    fn read_instruction<'a>(&mut self, instruction: Instruction<'a>);

    fn read_instructions<'a>(&mut self, instructions: Vec<Instruction<'a>>) {
        for instruction in instructions {
            self.read_instruction(instruction);
        }
    }
}
