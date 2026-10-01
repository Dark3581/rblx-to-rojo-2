use log::{info, warn};
use rbx_dom_weak::{
    types::{Ref, Variant},
    ustr, Instance, WeakDom,
};
use rbx_reflection::ClassTag;
use rbx_reflection_database::get_bundled;
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

use structures::*;

pub mod filesystem;
pub mod structures;

#[cfg(test)]
mod tests;

lazy_static::lazy_static! {
    static ref NON_TREE_SERVICES: HashSet<&'static str> = include_str!("./non-tree-services.txt").lines().collect();
    static ref RESPECTED_SERVICES: HashSet<&'static str> = include_str!("./respected-services.txt").lines().collect();
    static ref REFLECTION_DB: &'static rbx_reflection::ReflectionDatabase<'static> = get_bundled();
}

/// Children of services whose names can't be file names are written here, and
/// mapped back to their real names by nodes in default.project.json.
pub const NAMED_DIR: &str = "__named__";

/// Script-bearing instances Rojo can't represent at all (e.g. two siblings with
/// the same name directly inside a service) are saved here as .rbxm so nothing is
/// lost. default.project.json does not reference this folder.
pub const CONFLICTS_DIR: &str = "__conflicts__";

/// Characters Windows doesn't allow in file names. `/` and `\` also split paths.
const ILLEGAL_FILE_CHARS: &str = "<>:\"/\\|?*";

/// Suffixes Rojo reads meaning into when they end a script's file stem, e.g. a
/// ModuleScript named `Foo.server` would be written as `Foo.server.lua` and come
/// back as a Script named `Foo`.
const ROJO_SUFFIXES: &[&str] = &[
    ".server", ".client", ".plugin", ".local", ".legacy", ".meta", ".model", ".project",
];

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Write `.luau` files instead of `.lua`.
    pub luau: bool,
}

/// What happened to the place's scripts. Every script lands in exactly one of
/// written, in_bundles, in_conflicts or dropped.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub scripts_in_place: usize,
    pub scripts_written: usize,
    pub scripts_in_bundles: usize,
    pub scripts_in_conflicts: usize,
    pub scripts_dropped: usize,
    /// (instance path, file written, reason)
    pub bundles: Vec<(String, String, String)>,
    /// (instance path, file written)
    pub renamed: Vec<(String, String)>,
    /// (instance path, file written, reason)
    pub conflicts: Vec<(String, String, String)>,
    /// (service name, scripts inside)
    pub dropped_services: Vec<(String, usize)>,
    /// Instances that couldn't be encoded to .rbxm: (instance path, error)
    pub errors: Vec<(String, String)>,
}

impl Summary {
    pub fn scripts_accounted(&self) -> usize {
        self.scripts_written + self.scripts_in_bundles + self.scripts_in_conflicts + self.scripts_dropped
    }
}

fn is_script(class: &str) -> bool {
    matches!(class, "Script" | "LocalScript" | "ModuleScript")
}

fn script_extension(class: &str) -> &'static str {
    match class {
        "Script" => ".server",
        "LocalScript" => ".client",
        _ => "",
    }
}

fn script_source_slice(instance: &Instance) -> &[u8] {
    match instance.properties.get(&ustr("Source")) {
        Some(Variant::String(value)) => value.as_bytes(),
        Some(Variant::SharedString(value)) => value.as_ref(),
        Some(other) => {
            warn!("{}: unexpected Source type {:?}, writing an empty file", instance.name, other.ty());
            &[]
        }
        None => &[],
    }
}

/// Properties worth keeping on the instances we write: attributes and tags on
/// everything, plus RunContext and Enabled on scripts. Defaults are left out.
fn meta_properties(instance: &Instance) -> BTreeMap<String, Variant> {
    let mut properties = BTreeMap::new();
    let class = instance.class.as_str();

    match instance.properties.get(&ustr("Attributes")) {
        Some(Variant::Attributes(attributes)) if attributes.len() > 0 => {
            properties.insert("Attributes".to_owned(), Variant::Attributes(attributes.clone()));
        }
        _ => {}
    }

    match instance.properties.get(&ustr("Tags")) {
        Some(Variant::Tags(tags)) if tags.len() > 0 => {
            properties.insert("Tags".to_owned(), Variant::Tags(tags.clone()));
        }
        _ => {}
    }

    if class == "Script" {
        if let Some(Variant::Enum(run_context)) = instance.properties.get(&ustr("RunContext")) {
            if run_context.to_u32() != 0 {
                properties.insert("RunContext".to_owned(), Variant::Enum(*run_context));
            }
        }
    }

    if matches!(class, "Script" | "LocalScript") {
        // Older decoders keep the serialized name (Disabled), newer ones the canonical one (Enabled).
        let disabled = matches!(instance.properties.get(&ustr("Disabled")), Some(Variant::Bool(true)))
            || matches!(instance.properties.get(&ustr("Enabled")), Some(Variant::Bool(false)));
        if disabled {
            properties.insert("Enabled".to_owned(), Variant::Bool(false));
        }
    }

    properties
}

fn meta_file_contents<'a>(class_name: Option<String>, properties: BTreeMap<String, Variant>) -> Cow<'a, [u8]> {
    Cow::Owned(
        serde_json::to_string_pretty(&MetaFile {
            class_name,
            ignore_unknown_instances: true,
            properties,
        })
        .expect("couldn't serialize meta")
        .into_bytes(),
    )
}

/// Why `name` can't be used as-is for a file or folder, if it can't.
fn bad_name_reason(name: &str, class: &str) -> Option<String> {
    if name.is_empty() {
        return Some("the name is empty".to_owned());
    }

    if let Some(c) = name.chars().find(|c| ILLEGAL_FILE_CHARS.contains(*c) || c.is_control()) {
        return Some(format!("the name contains {:?}, which can't be in a file name", c));
    }

    if name.ends_with('.') || name.ends_with(' ') {
        return Some("the name ends with a dot or space, which Windows strips from file names".to_owned());
    }

    let lower = name.to_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Some("the name is reserved by Windows".to_owned());
    }

    if lower == "init" {
        return Some("Rojo would read a file named init as its parent's source".to_owned());
    }

    if is_script(class) {
        if let Some(suffix) = ROJO_SUFFIXES.iter().find(|suffix| lower.ends_with(*suffix)) {
            return Some(format!("Rojo would read the {} suffix as part of the file type", suffix));
        }
    }

    None
}

/// A version of `name` that's safe to use as a file name. Only used where the
/// project file maps it back to the real name, or for the conflicts folder.
fn sanitize_name(name: &str, class: &str) -> String {
    let mut sanitized: String = name
        .chars()
        .map(|c| if ILLEGAL_FILE_CHARS.contains(c) || c.is_control() { '_' } else { c })
        .collect();

    while sanitized.ends_with('.') || sanitized.ends_with(' ') {
        sanitized.pop();
        sanitized.push('_');
    }

    if sanitized.is_empty() {
        sanitized.push('_');
    }

    // Anything still flagged (reserved names, init, Rojo suffixes) gets its dots
    // replaced and a leading underscore.
    if bad_name_reason(&sanitized, class).is_some() {
        sanitized = format!("_{}", sanitized.replace('.', "_"));
    }

    sanitized
}

fn unique_name(base: String, used: &mut HashSet<String>) -> String {
    let mut candidate = base.clone();
    let mut n = 2;
    while !used.insert(candidate.to_lowercase()) {
        candidate = format!("{} ({})", base, n);
        n += 1;
    }
    candidate
}

/// Paths as shown in the log, with forward slashes on every platform.
pub fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn instance_path(tree: &WeakDom, mut referent: Ref) -> String {
    let mut parts = Vec::new();
    while referent != tree.root_ref() {
        match tree.get_by_ref(referent) {
            Some(instance) => {
                parts.push(instance.name.as_str());
                referent = instance.parent();
            }
            None => break,
        }
    }
    parts.reverse();
    parts.join("/")
}

fn count_scripts(tree: &WeakDom, referent: Ref) -> usize {
    let instance = tree.get_by_ref(referent).expect("fake child id?");
    let own = is_script(instance.class.as_str()) as usize;
    own + instance
        .children()
        .iter()
        .map(|child| count_scripts(tree, *child))
        .sum::<usize>()
}

fn check_has_scripts(
    tree: &WeakDom,
    instance: &Instance,
    has_scripts: &mut HashMap<Ref, bool>,
) -> bool {
    let mut children_have_scripts = false;

    for child_id in instance.children() {
        let result = check_has_scripts(
            tree,
            tree.get_by_ref(*child_id).expect("fake child id?"),
            has_scripts,
        );

        children_have_scripts = children_have_scripts || result;
    }

    let result = is_script(instance.class.as_str()) || children_have_scripts;
    has_scripts.insert(instance.referent(), result);
    result
}

/// How an instance ends up on disk.
#[derive(Clone, Debug, PartialEq)]
enum Plan {
    /// Not written: it has no scripts, or it's a service we don't sync.
    Skip,
    /// A service in respected-services.txt, with its own folder.
    Service,
    /// StarterPlayer: in the project file, but only its script-bearing children get folders.
    StarterPlayer,
    /// Script files and folders under its own name.
    Files,
    /// The whole subtree goes in `<name>.rbxm`, because its children can't be
    /// written as separate files.
    Bundle,
    /// A service child whose name can't be a file name. Written under NAMED_DIR
    /// as `fs_name` and mapped back to its real name in the project file.
    Named { fs_name: String, bundle: bool },
    /// Can't be represented in the project. Saved to CONFLICTS_DIR as `fs_name`.rbxm.
    Conflict { fs_name: String, reason: String },
}

/// Script files write either a single file (plus maybe a sidecar meta file) or
/// a folder with init files.
enum ScriptShape {
    File { meta: bool },
    Folder { meta: bool },
}

struct Converter<'a> {
    tree: &'a WeakDom,
    options: &'a Options,
    has_scripts: HashMap<Ref, bool>,
    plans: HashMap<Ref, Plan>,
    /// Instances whose children force them to become a bundle, with the reason.
    bundle_reasons: HashMap<Ref, String>,
    unknown_classes: HashSet<String>,
    created_folders: HashSet<PathBuf>,
    summary: Summary,
}

impl<'a> Converter<'a> {
    fn instance(&self, referent: Ref) -> &'a Instance {
        self.tree.get_by_ref(referent).expect("fake child id?")
    }

    fn has_scripts(&self, referent: Ref) -> bool {
        self.has_scripts.get(&referent) == Some(&true)
    }

    fn plan(&self, referent: Ref) -> &Plan {
        self.plans.get(&referent).unwrap_or(&Plan::Skip)
    }

    fn lua_extension(&self) -> &'static str {
        if self.options.luau {
            ".luau"
        } else {
            ".lua"
        }
    }

    fn script_shape(&self, instance: &Instance) -> ScriptShape {
        let has_properties = !meta_properties(instance).is_empty();
        let total_children = instance.children().len();
        let script_children = instance
            .children()
            .iter()
            .filter(|child| self.has_scripts(**child))
            .count();

        if total_children == 0 {
            // Just a script
            ScriptShape::File { meta: has_properties }
        } else if script_children == 0 {
            // Children, but no scripts: a file with a meta file so Rojo leaves the children alone
            ScriptShape::File { meta: true }
        } else if script_children == total_children {
            // Only script children: a folder, no meta needed
            ScriptShape::Folder { meta: has_properties }
        } else {
            // Some script children: a folder with a meta file
            ScriptShape::Folder { meta: true }
        }
    }

    /// The file or folder that represents `instance` when written as `fs_name`.
    fn main_entry(&self, instance: &Instance, fs_name: &str, bundle: bool) -> String {
        if bundle {
            return format!("{}.rbxm", fs_name);
        }

        let class = instance.class.as_str();
        if is_script(class) {
            if let ScriptShape::File { .. } = self.script_shape(instance) {
                return format!("{}{}{}", fs_name, script_extension(class), self.lua_extension());
            }
        }

        fs_name.to_owned()
    }

    /// Every file or folder name `instance` takes up in its parent folder.
    fn entries(&self, instance: &Instance) -> Vec<String> {
        let bundle = *self.plan(instance.referent()) == Plan::Bundle;
        let mut entries = vec![self.main_entry(instance, &instance.name, bundle)];

        if !bundle && is_script(instance.class.as_str()) {
            if let ScriptShape::File { meta: true } = self.script_shape(instance) {
                entries.push(format!("{}.meta.json", instance.name));
            }
        }

        entries
    }

    fn base_plan(&mut self, instance: &Instance) -> Plan {
        if !self.has_scripts(instance.referent()) {
            return Plan::Skip;
        }

        let class = instance.class.as_str();
        if class == "StarterPlayer" {
            // We can't respect StarterPlayer as a service, because then Rojo
            // tries to delete StarterPlayerScripts and whatnot, which is not valid.
            return Plan::StarterPlayer;
        }

        if is_script(class) || class == "Folder" {
            return Plan::Files;
        }

        match REFLECTION_DB.classes.get(class) {
            Some(reflected) => {
                if RESPECTED_SERVICES.contains(class) {
                    Plan::Service
                } else if reflected.tags.contains(&ClassTag::Service) {
                    let scripts = count_scripts(self.tree, instance.referent());
                    warn!(
                        "{} has {} script(s) but isn't in respected-services.txt, so they are NOT exported",
                        instance_path(self.tree, instance.referent()),
                        scripts
                    );
                    self.summary.scripts_dropped += scripts;
                    self.summary
                        .dropped_services
                        .push((instance_path(self.tree, instance.referent()), scripts));
                    Plan::Skip
                } else {
                    Plan::Files
                }
            }

            None => {
                if self.unknown_classes.insert(class.to_owned()) {
                    warn!(
                        "class {} isn't in the reflection database; treating it as a plain container",
                        class
                    );
                }
                Plan::Files
            }
        }
    }

    /// Decides the plan for every descendant of `parent`, bottom-up so a
    /// parent knows whether its children forced it to become a bundle.
    fn plan_children(&mut self, parent: Ref) {
        let children = self.instance(parent).children().to_vec();

        for &child in &children {
            let plan = self.base_plan(self.instance(child));
            let recurse = plan != Plan::Skip;
            self.plans.insert(child, plan);

            if recurse {
                self.plan_children(child);
                if self.bundle_reasons.contains_key(&child) {
                    self.plans.insert(child, Plan::Bundle);
                }
            }
        }

        let problems = self.find_problems(&children);
        if problems.is_empty() {
            return;
        }

        let parent_plan = self.plans.get(&parent).cloned();

        if parent_plan == Some(Plan::Files) {
            // Several children usually share a problem ("Light" x4), so list each once.
            let mut reasons: Vec<(String, usize)> = Vec::new();
            for (child, reason, _) in &problems {
                let reason = format!("{:?} {}", self.instance(*child).name, reason);
                match reasons.iter_mut().find(|(existing, _)| *existing == reason) {
                    Some((_, count)) => *count += 1,
                    None => reasons.push((reason, 1)),
                }
            }
            let reasons: Vec<String> = reasons
                .into_iter()
                .map(|(reason, count)| if count > 1 { format!("{} (x{})", reason, count) } else { reason })
                .collect();
            self.bundle_reasons.insert(parent, reasons.join("; "));
            return;
        }

        // The parent is a service or the root, which can't be bundled.
        let named_allowed = parent_plan == Some(Plan::Service);
        let mut used = HashSet::new();

        for (child, reason, is_collision) in problems {
            let instance = self.instance(child);
            let fs_name = unique_name(sanitize_name(&instance.name, instance.class.as_str()), &mut used);
            let bundle = *self.plan(child) == Plan::Bundle;

            let plan = if named_allowed && !is_collision {
                Plan::Named { fs_name, bundle }
            } else {
                Plan::Conflict {
                    fs_name,
                    reason: format!("{:?} {}", instance.name, reason),
                }
            };

            self.plans.insert(child, plan);
        }
    }

    /// Children that can't be written as files under their own names:
    /// (child, reason, whether it's a clash with a sibling).
    fn find_problems(&self, children: &[Ref]) -> Vec<(Ref, String, bool)> {
        let written: Vec<Ref> = children
            .iter()
            .copied()
            .filter(|child| matches!(self.plan(*child), Plan::Files | Plan::Bundle))
            .collect();

        let mut owners: HashMap<String, Vec<Ref>> = HashMap::new();
        for &child in &written {
            for entry in self.entries(self.instance(child)) {
                owners.entry(entry.to_lowercase()).or_default().push(child);
            }
        }

        let mut problems = Vec::new();

        for &child in &written {
            let instance = self.instance(child);

            if let Some(reason) = bad_name_reason(&instance.name, instance.class.as_str()) {
                problems.push((child, reason, false));
                continue;
            }

            let clashes_on_disk = self.entries(instance).iter().any(|entry| {
                owners
                    .get(&entry.to_lowercase())
                    .map_or(false, |owners| owners.len() > 1)
            });

            // Rojo matches existing instances by name and class when syncing, so
            // a same-named twin makes it ambiguous even if the twin has no scripts.
            let has_twin = children.iter().any(|other| {
                *other != child && {
                    let other = self.instance(*other);
                    other.name == instance.name && other.class == instance.class
                }
            });

            if clashes_on_disk {
                problems.push((child, "has a sibling that would be written to the same file name".to_owned(), true));
            } else if has_twin {
                problems.push((child, "has a sibling with the same name and class".to_owned(), true));
            }
        }

        problems
    }

    fn create_folder(&mut self, instructions: &mut Vec<Instruction<'a>>, folder: PathBuf) {
        if self.created_folders.insert(folder.clone()) {
            instructions.push(Instruction::CreateFolder {
                folder: Cow::Owned(folder),
            });
        }
    }

    fn bundle_file(&mut self, instructions: &mut Vec<Instruction<'a>>, instance: &Instance, filename: PathBuf) -> bool {
        let mut contents = Vec::new();
        match rbx_binary::to_writer(&mut contents, self.tree, &[instance.referent()]) {
            Ok(()) => {
                instructions.push(Instruction::CreateFile {
                    filename: Cow::Owned(filename),
                    contents: Cow::Owned(contents),
                });
                true
            }

            Err(error) => {
                let path = instance_path(self.tree, instance.referent());
                log::error!("{}: couldn't encode as .rbxm: {}", path, error);
                self.summary.errors.push((path, error.to_string()));
                false
            }
        }
    }

    /// Instructions for a script or container written as files named `fs_name`
    /// inside `base`. Returns the folder its children go in.
    fn files_instructions(
        &mut self,
        instructions: &mut Vec<Instruction<'a>>,
        base: &Path,
        instance: &'a Instance,
        fs_name: &str,
    ) -> PathBuf {
        let class = instance.class.as_str();
        let properties = meta_properties(instance);

        if !is_script(class) {
            let folder = base.join(fs_name);
            self.create_folder(instructions, folder.clone());
            instructions.push(Instruction::CreateFile {
                filename: Cow::Owned(folder.join("init.meta.json")),
                contents: meta_file_contents(
                    (class != "Folder").then(|| class.to_owned()),
                    properties,
                ),
            });
            return folder;
        }

        self.summary.scripts_written += 1;
        let extension = format!("{}{}", script_extension(class), self.lua_extension());
        let source = Cow::Borrowed(script_source_slice(instance));

        match self.script_shape(instance) {
            ScriptShape::File { meta } => {
                instructions.push(Instruction::CreateFile {
                    filename: Cow::Owned(base.join(format!("{}{}", fs_name, extension))),
                    contents: source,
                });

                if meta {
                    instructions.push(Instruction::CreateFile {
                        filename: Cow::Owned(base.join(format!("{}.meta.json", fs_name))),
                        contents: meta_file_contents(None, properties),
                    });
                }

                base.to_path_buf()
            }

            ScriptShape::Folder { meta } => {
                let folder = base.join(fs_name);
                self.create_folder(instructions, folder.clone());
                instructions.push(Instruction::CreateFile {
                    filename: Cow::Owned(folder.join(format!("init{}", extension))),
                    contents: source,
                });

                if meta {
                    instructions.push(Instruction::CreateFile {
                        filename: Cow::Owned(folder.join("init.meta.json")),
                        contents: meta_file_contents(None, properties),
                    });
                }

                folder
            }
        }
    }

    /// Project file nodes that map NAMED_DIR files back to their real names,
    /// for the children of the service stored in `folder`.
    fn named_partitions(&self, service: &Instance, folder: &Path) -> BTreeMap<String, TreePartition> {
        service
            .children()
            .iter()
            .filter_map(|child| match self.plan(*child) {
                Plan::Named { fs_name, bundle } => {
                    let instance = self.instance(*child);
                    Some((
                        instance.name.clone(),
                        TreePartition {
                            class_name: None,
                            children: BTreeMap::new(),
                            ignore_unknown_instances: true,
                            path: Some(
                                PathBuf::from(NAMED_DIR)
                                    .join(folder)
                                    .join(self.main_entry(instance, fs_name, *bundle)),
                            ),
                        },
                    ))
                }
                _ => None,
            })
            .collect()
    }

    fn service_partition(&self, service: &Instance, folder: &Path) -> TreePartition {
        TreePartition {
            class_name: Some(service.class.to_string()),
            children: self.named_partitions(service, folder),
            ignore_unknown_instances: true,
            path: Some(folder.to_path_buf()),
        }
    }

    fn visit<I: InstructionReader + ?Sized>(&mut self, reader: &mut I, parent: &'a Instance, path: &Path) {
        for child_id in parent.children() {
            let child = self.instance(*child_id);
            let plan = self.plan(*child_id).clone();
            let mut instructions = Vec::new();

            let child_path = match plan {
                Plan::Skip => continue,

                Plan::Service => {
                    let folder = path.join(&child.name);
                    if !NON_TREE_SERVICES.contains(child.class.as_str()) {
                        instructions.push(Instruction::AddToTree {
                            name: child.name.clone(),
                            partition: self.service_partition(child, &folder),
                        });
                    }
                    self.create_folder(&mut instructions, folder.clone());
                    Some(folder)
                }

                Plan::StarterPlayer => {
                    let folder = path.join(&child.name);
                    self.create_folder(&mut instructions, folder.clone());

                    let children = child
                        .children()
                        .iter()
                        .filter_map(|id| {
                            let instance = self.instance(*id);
                            let partition = match self.plan(*id) {
                                Plan::Service => self.service_partition(instance, &folder.join(&instance.name)),
                                Plan::Files | Plan::Bundle => TreePartition {
                                    class_name: None,
                                    children: BTreeMap::new(),
                                    ignore_unknown_instances: true,
                                    path: Some(folder.join(self.main_entry(
                                        instance,
                                        &instance.name,
                                        *self.plan(*id) == Plan::Bundle,
                                    ))),
                                },
                                _ => return None,
                            };
                            Some((instance.name.clone(), partition))
                        })
                        .collect();

                    instructions.push(Instruction::AddToTree {
                        name: child.name.clone(),
                        partition: TreePartition {
                            class_name: Some(child.class.to_string()),
                            children,
                            ignore_unknown_instances: true,
                            path: None,
                        },
                    });

                    Some(folder)
                }

                Plan::Files => Some(self.files_instructions(&mut instructions, path, child, &child.name)),

                Plan::Bundle => {
                    let filename = path.join(format!("{}.rbxm", child.name));
                    if self.bundle_file(&mut instructions, child, filename.clone()) {
                        let reason = self.bundle_reasons.get(child_id).cloned().unwrap_or_default();
                        let instance_path = instance_path(self.tree, *child_id);
                        warn!(
                            "{} written as {} because {}. Scripts inside aren't plain files; rename the duplicates in Studio and re-run to get them as files.",
                            instance_path,
                            display_path(&filename),
                            reason
                        );
                        self.summary.scripts_in_bundles += count_scripts(self.tree, *child_id);
                        self.summary
                            .bundles
                            .push((instance_path, display_path(&filename), reason));
                    }
                    None
                }

                Plan::Named { fs_name, bundle } => {
                    let base = PathBuf::from(NAMED_DIR).join(path);
                    self.create_folder(&mut instructions, base.clone());
                    let written = base.join(self.main_entry(child, &fs_name, bundle));
                    let instance_path = instance_path(self.tree, *child_id);
                    warn!(
                        "{} can't be a file name ({}); written as {} and mapped back to its real name in default.project.json",
                        instance_path,
                        bad_name_reason(&child.name, child.class.as_str()).unwrap_or_default(),
                        display_path(&written)
                    );
                    self.summary
                        .renamed
                        .push((instance_path, display_path(&written)));

                    if bundle {
                        if self.bundle_file(&mut instructions, child, written) {
                            self.summary.scripts_in_bundles += count_scripts(self.tree, *child_id);
                        }
                        None
                    } else {
                        Some(self.files_instructions(&mut instructions, &base, child, &fs_name))
                    }
                }

                Plan::Conflict { fs_name, reason } => {
                    let base = PathBuf::from(CONFLICTS_DIR).join(path);
                    self.create_folder(&mut instructions, base.clone());
                    let filename = base.join(format!("{}.rbxm", fs_name));
                    if self.bundle_file(&mut instructions, child, filename.clone()) {
                        let instance_path = instance_path(self.tree, *child_id);
                        warn!(
                            "{} can't be represented in a Rojo project because {}. Saved to {} but NOT synced; rename it in Studio and re-run.",
                            instance_path,
                            reason,
                            display_path(&filename)
                        );
                        self.summary.scripts_in_conflicts += count_scripts(self.tree, *child_id);
                        self.summary
                            .conflicts
                            .push((instance_path, display_path(&filename), reason));
                    }
                    None
                }
            };

            reader.read_instructions(instructions);

            if let Some(child_path) = child_path {
                self.visit(reader, child, &child_path);
            }
        }
    }
}

pub fn process_instructions(tree: &WeakDom, instruction_reader: &mut dyn InstructionReader) -> Summary {
    process_instructions_with(tree, instruction_reader, &Options::default())
}

pub fn process_instructions_with(
    tree: &WeakDom,
    instruction_reader: &mut dyn InstructionReader,
    options: &Options,
) -> Summary {
    let root = tree.root();

    let mut has_scripts = HashMap::new();
    check_has_scripts(tree, root, &mut has_scripts);

    let mut converter = Converter {
        tree,
        options,
        has_scripts,
        plans: HashMap::new(),
        bundle_reasons: HashMap::new(),
        unknown_classes: HashSet::new(),
        created_folders: HashSet::new(),
        summary: Summary {
            scripts_in_place: count_scripts(tree, tree.root_ref()),
            ..Summary::default()
        },
    };

    converter.plan_children(tree.root_ref());
    converter.visit(instruction_reader, root, Path::new(""));
    instruction_reader.finish_instructions();

    let summary = converter.summary;

    if summary.scripts_in_place == 0 {
        warn!("The place has no scripts, so there's nothing to export.");
    }

    info!(
        "Scripts in place: {} | written as files: {} | inside .rbxm bundles: {} | saved to {} (not synced): {} | in ignored services: {}",
        summary.scripts_in_place,
        summary.scripts_written,
        summary.scripts_in_bundles,
        CONFLICTS_DIR,
        summary.scripts_in_conflicts,
        summary.scripts_dropped,
    );

    if summary.scripts_accounted() != summary.scripts_in_place {
        warn!(
            "{} script(s) are unaccounted for; see the errors above",
            summary.scripts_in_place as isize - summary.scripts_accounted() as isize
        );
    }

    summary
}
