use crate::{filesystem::FileSystem, process_instructions, structures::*};
use log::info;
use pretty_assertions::assert_eq;
use rbx_dom_weak::{
    types::{Ref, Variant},
    ustr, WeakDom,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::ErrorKind,
    time::Instant,
};

#[derive(Deserialize, Serialize, Debug, PartialEq)]
enum VirtualFileContents {
    Bytes(String),
    Instance(HashMap<String, Variant>),
    /// A .rbxm bundle, as one line per instance: path, class, and script source.
    Model(Vec<String>),
    Vfs(VirtualFileSystem),
}

#[derive(Deserialize, Serialize, Debug, PartialEq)]
struct VirtualFile {
    contents: VirtualFileContents,
}

#[derive(Deserialize, Serialize, Debug, Default)]
struct VirtualFileSystem {
    files: BTreeMap<String, VirtualFile>,
    tree: BTreeMap<String, TreePartition>,
    #[serde(skip)]
    finished: bool,
}

impl PartialEq<VirtualFileSystem> for VirtualFileSystem {
    fn eq(&self, rhs: &VirtualFileSystem) -> bool {
        self.files == rhs.files && self.tree == rhs.tree
    }
}

impl InstructionReader for VirtualFileSystem {
    fn finish_instructions(&mut self) {
        self.finished = true;
    }

    fn read_instruction<'a>(&mut self, instruction: Instruction<'a>) {
        match instruction {
            Instruction::AddToTree { name, partition } => {
                self.tree.insert(name, partition);
            }

            Instruction::CreateFile { filename, contents } => {
                let parent = filename
                    .parent()
                    .expect("no parent?")
                    .to_string_lossy()
                    .replace("\\", "/");
                let filename = filename
                    .file_name()
                    .expect("no filename?")
                    .to_string_lossy()
                    .replace("\\", "/");

                let system = if parent == "" {
                    self
                } else {
                    match self
                        .files
                        .get_mut(&parent)
                        .unwrap_or_else(|| panic!("no folder for {:?}", parent))
                        .contents
                    {
                        VirtualFileContents::Vfs(ref mut system) => system,
                        _ => unreachable!("attempt to parent to a file"),
                    }
                };

                let contents_string = String::from_utf8_lossy(&contents).into_owned();
                let rbxmx = filename.ends_with(".rbxmx");
                let rbxm = filename.ends_with(".rbxm");
                system.files.insert(
                    filename,
                    VirtualFile {
                        contents: if rbxm {
                            let tree = rbx_binary::from_reader(&contents[..])
                                .expect("couldn't decode encoded rbxm");
                            let mut lines = Vec::new();
                            describe_model(&tree, tree.root_ref(), "", &mut lines);
                            VirtualFileContents::Model(lines)
                        } else if rbxmx {
                            let tree = rbx_xml::from_str_default(&contents_string)
                                .expect("couldn't decode encoded xml");
                            let child_id = tree.root().children()[0];
                            let child_instance = tree.get_by_ref(child_id).unwrap().clone();
                            let props: HashMap<String, Variant> = child_instance
                                .properties
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.clone()))
                                .collect();
                            VirtualFileContents::Instance(props)
                        } else {
                            VirtualFileContents::Bytes(contents_string)
                        },
                    },
                );
            }

            Instruction::CreateFolder { folder } => {
                let name = folder.to_string_lossy().replace("\\", "/");
                self.files.insert(
                    name,
                    VirtualFile {
                        contents: VirtualFileContents::Vfs(VirtualFileSystem::default()),
                    },
                );
            }
        }
    }
}

fn describe_model(tree: &WeakDom, referent: Ref, prefix: &str, lines: &mut Vec<String>) {
    for child in tree.get_by_ref(referent).unwrap().children() {
        let instance = tree.get_by_ref(*child).unwrap();
        let path = format!("{}/{}", prefix, instance.name);
        match instance.properties.get(&ustr("Source")) {
            Some(Variant::String(source)) => {
                lines.push(format!("{} [{}] {:?}", path, instance.class, source))
            }
            _ => lines.push(format!("{} [{}]", path, instance.class)),
        }
        describe_model(tree, *child, &path, lines);
    }
}

#[test]
fn run_tests() {
    let _ = env_logger::init();
    for entry in fs::read_dir("./test-files").expect("couldn't read test-files") {
        let entry = entry.unwrap();
        let path = entry.path();
        info!("testing {:?}", path);

        let mut source_path = path.clone();
        source_path.push("source.rbxmx");
        let source = fs::read_to_string(&source_path).expect("couldn't read source.rbxmx");

        let time = Instant::now();
        let tree = rbx_xml::from_str_default(&source).expect("couldn't deserialize source.rbxmx");
        info!(
            "decoding for {:?} took {}ms",
            path,
            Instant::now().duration_since(time).as_millis()
        );

        let mut vfs = VirtualFileSystem::default();
        let time = Instant::now();
        let summary = process_instructions(&tree, &mut vfs);
        assert_eq!(
            summary.scripts_accounted(),
            summary.scripts_in_place,
            "every script in {:?} should be written, bundled, conflicted or dropped: {:?}",
            path,
            summary
        );
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        info!(
            "processing instructions for {:?} took {}ms",
            path,
            Instant::now().duration_since(time).as_millis()
        );

        let mut expected_path = path.clone();
        expected_path.push("output.json");
        assert!(vfs.finished, "finish_instructions was not called");

        if let Ok(expected) = fs::read_to_string(&expected_path) {
            assert_eq!(
                serde_json::from_str::<VirtualFileSystem>(&expected).unwrap(),
                vfs,
            );
        } else {
            let output = serde_json::to_string_pretty(&vfs).unwrap();
            fs::write(&expected_path, output).expect("couldn't write to output.json");
        }

        let filesystem_path = path.join("filesystem");
        if let Err(error) = fs::remove_dir_all(&filesystem_path) {
            match error.kind() {
                ErrorKind::NotFound => {}
                other => panic!("couldn't remove filesystem dir: {:?}", other),
            }
        }

        fs::create_dir(&filesystem_path).unwrap();

        let mut filesystem = FileSystem::from_root(filesystem_path);
        process_instructions(&tree, &mut filesystem);
        assert!(filesystem.errors().is_empty(), "{:?}", filesystem.errors());
    }
}
