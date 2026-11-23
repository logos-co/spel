//! Generates an IDL from a program's core crate, with no annotations in the program.
//!
//! The caller names the three things a client needs: the message enum, the state type (or
//! types) and the module holding the events (`spel generate-idl programs/token/core --message
//! Message --state TokenState --events event`). Each can instead be marked in the crate with
//! an inert attribute from `spel-attrs`: `#[message]` on the enum, `#[state]` on the state
//! types, `#[event]` on the trait the events implement. Everything else is read from plain Rust
//! types: the enum's variant order gives the Borsh tags, doc comments become help, and every
//! struct or enum those types reference is described, following path dependencies.
//!
//! A name that doesn't exist is an error, so a rename breaks generation instead of silently
//! dropping a type.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use syn::{Attribute, Fields, Item, Type};

use crate::idl::{
    event_selector, IdlEnumVariant, IdlEvent, IdlField, IdlMessage, IdlType, IdlTypeDef, SpelIdl,
    IDL_VERSION,
};
use crate::source::{crate_items, SourceItem};

/// What to read from the crate.
#[derive(Debug, Clone, Default)]
pub struct GenOptions {
    /// The core crate's directory (holding `Cargo.toml` and `src/lib.rs`).
    pub crate_dir: PathBuf,
    /// The external message enum, e.g. `Message`. `None`: the enum the crate marks
    /// `#[message]`.
    pub message: Option<String>,
    /// The actor state types, usually one (e.g. `TokenState`); several for a program that
    /// stores different untagged structs on different actors. They become the IDL's
    /// `state_types`, in this order, without duplicates. Empty: the crate's `#[state]` markers,
    /// in declaration order.
    pub states: Vec<String>,
    /// The module holding the event structs, e.g. `event` (nested: `a::b`). `None`: the impls
    /// of the traits the crate marks `#[event]`, or no events if none is marked.
    pub events: Option<String>,
    /// The program name and event namespace. Defaults to the crate name without `_core`.
    pub namespace: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdlGenError {
    #[error("{0}")]
    Source(String),
    #[error("{crate_dir}: no {kind} named `{name}` in the crate")]
    NotFound {
        crate_dir: String,
        kind: &'static str,
        name: String,
    },
    #[error("`{name}` is not an enum; --message names the program's message enum")]
    MessageNotEnum { name: String },
    #[error(
        "no message enum: pass --message, set `message` in spel.toml's [idl-gen], or mark the \
         enum #[message]"
    )]
    NoMessage,
    #[error("several enums are marked #[message] ({names}); mark only the external one")]
    SeveralMessages { names: String },
    #[error("`{name}` has {count} variants; a Borsh tag holds at most 256")]
    TooManyVariants { name: String, count: usize },
    #[error("no events in module `{module}`: expected `impl <Trait> for <Struct> {{ const NAME: &str = \"..\"; }}`")]
    NoEvents { module: String },
    #[error("nothing implements `{trait_name}`, marked #[event]")]
    UnimplementedEventTrait { trait_name: String },
    #[error(
        "`impl {trait_name} for {struct_name}` has no `const NAME: &str = \"..\"`; an event's name \
         must be a string literal"
    )]
    EventWithoutName {
        trait_name: String,
        struct_name: String,
    },
    #[error("two events are named `{name}`; their selectors would collide")]
    DuplicateEvent { name: String },
    #[error(
        "type `{name}` (used by `{used_by}`) is not defined in the crate or its path dependencies"
    )]
    UnknownType { name: String, used_by: String },
    #[error("cannot describe `{ty}` (in `{used_by}`): {reason}")]
    Unsupported {
        ty: String,
        used_by: String,
        reason: &'static str,
    },
}

/// Generate the IDL described by `options`. `on_warning` hears about dependency crates that
/// could not be read; their types are then missing, which an unknown-type error names.
///
/// # Errors
/// See [`IdlGenError`].
pub fn generate_idl<F: FnMut(String)>(
    options: &GenOptions,
    mut on_warning: F,
) -> Result<SpelIdl, IdlGenError> {
    let crate_dir = &options.crate_dir;
    let own = crate_items(crate_dir).map_err(IdlGenError::Source)?;
    let namespace = match &options.namespace {
        Some(namespace) => namespace.clone(),
        None => default_namespace(crate_dir).map_err(IdlGenError::Source)?,
    };

    let mut index = TypeIndex::default();
    index.add(&own);
    let graph = crate::dep_walk::resolve_dep_graph(crate_dir, false, &mut on_warning);
    for dir in &graph.transitive_dirs {
        match crate_items(dir) {
            Ok(items) => index.add(&items),
            Err(error) => on_warning(format!("skipping dependency {}: {error}", dir.display())),
        }
    }

    let not_found = |kind, name: &str| IdlGenError::NotFound {
        crate_dir: crate_dir.display().to_string(),
        kind,
        name: name.to_owned(),
    };

    let mut collector = Collector::new(&index);

    // A named message enum wins; without one, the crate's `#[message]` marker says.
    let marked_messages = marked(&own, "message");
    let message_name = match (&options.message, marked_messages.as_slice()) {
        (Some(named), markers) => {
            if !markers.is_empty() && markers != [named.clone()] {
                on_warning(format!(
                    "using the named message enum `{named}`, not the #[message] marker ({})",
                    markers.join(", ")
                ));
            }
            named.clone()
        },
        (None, [one]) => one.clone(),
        (None, []) => return Err(IdlGenError::NoMessage),
        (None, several) => {
            return Err(IdlGenError::SeveralMessages {
                names: several.join(", "),
            })
        },
    };
    let message_enum = match index.own_definition(&message_name) {
        Some(Item::Enum(e)) => e,
        Some(_) => {
            return Err(IdlGenError::MessageNotEnum {
                name: message_name.clone(),
            })
        },
        None => return Err(not_found("enum", &message_name)),
    };
    if message_enum.variants.len() > 256 {
        return Err(IdlGenError::TooManyVariants {
            name: message_name.clone(),
            count: message_enum.variants.len(),
        });
    }
    let mut messages = Vec::new();
    for (tag, variant) in (0_u8..=u8::MAX).zip(&message_enum.variants) {
        let used_by = format!("{message_name}::{}", variant.ident);
        messages.push(IdlMessage {
            name: variant.ident.to_string(),
            tag,
            docs: docs(&variant.attrs),
            fields: collector.fields(&variant.fields, &used_by)?,
        });
    }

    // Named state types win; without any, the crate's `#[state]` markers say.
    let marked_states = marked(&own, "state");
    let (named, source) = if options.states.is_empty() {
        (&marked_states, "#[state]")
    } else {
        let mut explicit = options.states.clone();
        explicit.sort();
        explicit.dedup();
        let mut markers = marked_states.clone();
        markers.sort();
        if !marked_states.is_empty() && explicit != markers {
            on_warning(format!(
                "using the named state types ({}), not the #[state] markers ({})",
                options.states.join(", "),
                marked_states.join(", ")
            ));
        }
        (&options.states, "--state")
    };
    let mut state_types: Vec<String> = Vec::new();
    for name in named {
        if state_types.contains(name) {
            continue;
        }
        if index.own_definition(name).is_none() {
            return Err(not_found("type", name));
        }
        collector.queue(name, source);
        state_types.push(name.clone());
    }

    // A named events module wins; without one, the impls of the crate's `#[event]` traits.
    let event_traits = marked(&own, "event");
    let found = match &options.events {
        Some(module) => {
            let path: Vec<String> = module.split("::").map(str::to_owned).collect();
            let found = module_events(&own, &path);
            if found.is_empty() {
                return Err(IdlGenError::NoEvents {
                    module: module.clone(),
                });
            }
            let mut named: Vec<&str> = found.iter().map(|(_, s)| s.as_str()).collect();
            named.sort_unstable();
            let marker_impls = trait_impls(&own, &event_traits);
            let mut markers: Vec<&str> = marker_impls.iter().map(|i| i.struct_name.as_str()).collect();
            markers.sort_unstable();
            if !event_traits.is_empty() && named != markers {
                on_warning(format!(
                    "using the events in module `{module}`, not the impls of the #[event] trait ({})",
                    event_traits.join(", ")
                ));
            }
            found
        },
        None => marked_events(&own, &event_traits)?,
    };
    let source = if options.events.is_some() { "--events" } else { "#[event]" };
    let mut events: Vec<IdlEvent> = Vec::new();
    for (name, struct_name) in found {
        if events.iter().any(|e| e.name == name) {
            return Err(IdlGenError::DuplicateEvent { name });
        }
        collector.queue(&struct_name, source);
        events.push(IdlEvent {
            selector: event_selector(&namespace, &name),
            name,
            type_: IdlType::Defined {
                defined: struct_name,
            },
        });
    }

    let types = collector.finish()?;
    Ok(SpelIdl {
        version: IDL_VERSION.to_owned(),
        name: namespace,
        messages,
        state_types,
        events,
        types,
    })
}

/// The structs and enums of the crate marked `#[<marker>]` or `#[spel_attrs::<marker>]`, in
/// declaration order (a module's items where its `mod` is declared). A marker imported under
/// another name isn't recognized.
fn marked(items: &[SourceItem], marker: &str) -> Vec<String> {
    let is_marker = |attr: &Attribute| {
        let path: Vec<String> = attr
            .path()
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        match path.as_slice() {
            [name] => name == marker,
            [krate, name] => krate == "spel_attrs" && name == marker,
            _ => false,
        }
    };
    items
        .iter()
        .filter_map(|source| match &source.item {
            Item::Struct(s) if s.attrs.iter().any(is_marker) => Some(s.ident.to_string()),
            Item::Enum(e) if e.attrs.iter().any(is_marker) => Some(e.ident.to_string()),
            Item::Trait(t) if t.attrs.iter().any(is_marker) => Some(t.ident.to_string()),
            _ => None,
        })
        .collect()
}

/// The crate name without a `_core` suffix: `token_core` → `token`.
fn default_namespace(crate_dir: &Path) -> Result<String, String> {
    let manifest = crate_dir.join("Cargo.toml");
    let content = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    let value: toml::Value = toml::from_str(&content)
        .map_err(|e| format!("cannot parse {}: {e}", manifest.display()))?;
    let name = value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| format!("{} has no package name", manifest.display()))?;
    Ok(name.strip_suffix("_core").unwrap_or(name).to_owned())
}

/// `(event name, struct name)` for every `impl <Trait> for <Struct> { const NAME: &str = ".."; }`
/// in the module at `path`, in source order. Trait impls without such a `NAME` are skipped.
fn module_events(items: &[SourceItem], path: &[String]) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|source| source.module == path)
        .filter_map(|source| match &source.item {
            Item::Impl(imp) if imp.trait_.is_some() => {
                let struct_name = last_ident(&imp.self_ty)?;
                Some((event_name(imp)?, struct_name))
            },
            _ => None,
        })
        .collect()
}

/// An impl of one of the `#[event]` traits.
struct TraitImpl {
    trait_name: String,
    struct_name: String,
    name: Option<String>,
}

/// Every `impl <Trait> for <Struct>` in the crate whose trait is one of `traits`, in
/// declaration order. The trait is matched by its last path segment.
fn trait_impls(items: &[SourceItem], traits: &[String]) -> Vec<TraitImpl> {
    items
        .iter()
        .filter_map(|source| match &source.item {
            Item::Impl(imp) => {
                let (_, trait_path, _) = imp.trait_.as_ref()?;
                let trait_name = trait_path.segments.last()?.ident.to_string();
                if !traits.contains(&trait_name) {
                    return None;
                }
                Some(TraitImpl {
                    trait_name,
                    struct_name: last_ident(&imp.self_ty)?,
                    name: event_name(imp),
                })
            },
            _ => None,
        })
        .collect()
}

/// `(event name, struct name)` for the impls of the `#[event]` traits. Each marked trait needs
/// at least one impl, and each impl a literal `NAME`.
fn marked_events(
    items: &[SourceItem],
    traits: &[String],
) -> Result<Vec<(String, String)>, IdlGenError> {
    let impls = trait_impls(items, traits);
    if let Some(unused) = traits
        .iter()
        .find(|t| !impls.iter().any(|i| &i.trait_name == *t))
    {
        return Err(IdlGenError::UnimplementedEventTrait {
            trait_name: unused.clone(),
        });
    }
    impls
        .into_iter()
        .map(|i| match i.name {
            Some(name) => Ok((name, i.struct_name)),
            None => Err(IdlGenError::EventWithoutName {
                trait_name: i.trait_name,
                struct_name: i.struct_name,
            }),
        })
        .collect()
}

/// The string literal of the impl's `const NAME`.
fn event_name(imp: &syn::ItemImpl) -> Option<String> {
    imp.items.iter().find_map(|item| match item {
        syn::ImplItem::Const(c) if c.ident == "NAME" => string_literal(&c.expr),
        _ => None,
    })
}

fn string_literal(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

fn last_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

/// `///` comments, one line each, trimmed; `None` when there are none.
fn docs(attrs: &[Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => string_literal(&nv.value),
            _ => None,
        })
        .map(|line| {
            line.strip_prefix(' ')
                .unwrap_or(&line)
                .trim_end()
                .to_owned()
        })
        .collect();
    let text = lines.join("\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Structs, enums and type aliases by name. The program's own crate wins over its
/// dependencies; within a crate the first definition wins.
#[derive(Default)]
struct TypeIndex {
    items: HashMap<String, (Item, bool)>,
    own_done: bool,
}

impl TypeIndex {
    fn add(&mut self, items: &[SourceItem]) {
        let own = !self.own_done;
        self.own_done = true;
        for source in items {
            let name = match &source.item {
                Item::Struct(s) => s.ident.to_string(),
                Item::Enum(e) => e.ident.to_string(),
                Item::Type(t) => t.ident.to_string(),
                _ => continue,
            };
            self.items
                .entry(name)
                .or_insert_with(|| (source.item.clone(), own));
        }
    }

    fn get(&self, name: &str) -> Option<&Item> {
        self.items.get(name).map(|(item, _)| item)
    }

    fn own_definition(&self, name: &str) -> Option<&Item> {
        self.items
            .get(name)
            .filter(|(_, own)| *own)
            .map(|(item, _)| item)
    }
}

/// Walks the types the messages, state and events reference and describes each once.
struct Collector<'a> {
    index: &'a TypeIndex,
    pending: VecDeque<(String, String)>,
    done: BTreeMap<String, IdlTypeDef>,
}

impl<'a> Collector<'a> {
    fn new(index: &'a TypeIndex) -> Self {
        Self {
            index,
            pending: VecDeque::new(),
            done: BTreeMap::new(),
        }
    }

    fn queue(&mut self, name: &str, used_by: &str) {
        if !self.done.contains_key(name) {
            self.pending
                .push_back((name.to_owned(), used_by.to_owned()));
        }
    }

    fn finish(mut self) -> Result<Vec<IdlTypeDef>, IdlGenError> {
        while let Some((name, used_by)) = self.pending.pop_front() {
            if self.done.contains_key(&name) {
                continue;
            }
            let def = match self.index.get(&name).cloned() {
                Some(Item::Struct(s)) => IdlTypeDef {
                    name: name.clone(),
                    kind: "struct".to_owned(),
                    fields: self.fields(&s.fields, &name)?,
                    variants: vec![],
                },
                Some(Item::Enum(e)) => {
                    let mut variants = Vec::new();
                    for variant in &e.variants {
                        let used_by = format!("{name}::{}", variant.ident);
                        variants.push(IdlEnumVariant {
                            name: variant.ident.to_string(),
                            fields: self.fields(&variant.fields, &used_by)?,
                        });
                    }
                    IdlTypeDef {
                        name: name.clone(),
                        kind: "enum".to_owned(),
                        fields: vec![],
                        variants,
                    }
                },
                // Aliases are resolved where they are used and never need a definition.
                Some(_) | None => return Err(IdlGenError::UnknownType { name, used_by }),
            };
            self.done.insert(name, def);
        }
        Ok(self.done.into_values().collect())
    }

    fn fields(&mut self, fields: &Fields, used_by: &str) -> Result<Vec<IdlField>, IdlGenError> {
        let mut out = Vec::new();
        for (position, field) in fields.iter().enumerate() {
            let name = field
                .ident
                .as_ref()
                .map_or_else(|| position.to_string(), ToString::to_string);
            out.push(IdlField {
                name,
                type_: self.idl_type(&field.ty, used_by, 0)?,
                docs: docs(&field.attrs),
            });
        }
        Ok(out)
    }

    fn idl_type(&mut self, ty: &Type, used_by: &str, depth: u8) -> Result<IdlType, IdlGenError> {
        let unsupported = |reason| IdlGenError::Unsupported {
            ty: quote_type(ty),
            used_by: used_by.to_owned(),
            reason,
        };
        if depth > 16 {
            return Err(unsupported("type aliases nest too deep"));
        }
        match ty {
            Type::Path(path) => {
                let segment = path
                    .path
                    .segments
                    .last()
                    .ok_or_else(|| unsupported("empty path"))?;
                let ident = segment.ident.to_string();
                let first_arg = || match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        args.args.iter().find_map(|arg| match arg {
                            syn::GenericArgument::Type(inner) => Some(inner.clone()),
                            _ => None,
                        })
                    },
                    _ => None,
                };
                match ident.as_str() {
                    "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                    | "i128" | "bool" => Ok(IdlType::Primitive(ident)),
                    "String" => Ok(IdlType::Primitive("string".to_owned())),
                    "AccountId" => Ok(IdlType::Primitive("account_id".to_owned())),
                    "ProgramId" => Ok(IdlType::Primitive("program_id".to_owned())),
                    // `lee_core::program::PdaSeed`: a newtype over 32 bytes, which is its Borsh.
                    "PdaSeed" => Ok(IdlType::Array {
                        array: (Box::new(IdlType::Primitive("u8".to_owned())), 32),
                    }),
                    "Vec" => {
                        let inner = first_arg().ok_or_else(|| unsupported("Vec without a type"))?;
                        Ok(IdlType::Vec {
                            vec: Box::new(self.idl_type(&inner, used_by, depth)?),
                        })
                    },
                    "Option" => {
                        let inner =
                            first_arg().ok_or_else(|| unsupported("Option without a type"))?;
                        Ok(IdlType::Option {
                            option: Box::new(self.idl_type(&inner, used_by, depth)?),
                        })
                    },
                    "Box" => {
                        let inner = first_arg().ok_or_else(|| unsupported("Box without a type"))?;
                        self.idl_type(&inner, used_by, depth)
                    },
                    _ => match self.index.get(&ident) {
                        Some(Item::Type(alias)) => {
                            let target = (*alias.ty).clone();
                            self.idl_type(&target, used_by, depth + 1)
                        },
                        _ => {
                            self.queue(&ident, used_by);
                            Ok(IdlType::Defined { defined: ident })
                        },
                    },
                }
            },
            Type::Array(array) => {
                let len = match &array.len {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Int(n),
                        ..
                    }) => n
                        .base10_parse::<usize>()
                        .ok()
                        .ok_or_else(|| unsupported("array length is not a number"))?,
                    _ => return Err(unsupported("array length must be a literal")),
                };
                Ok(IdlType::Array {
                    array: (Box::new(self.idl_type(&array.elem, used_by, depth)?), len),
                })
            },
            _ => Err(unsupported("only paths and arrays are supported")),
        }
    }
}

fn quote_type(ty: &Type) -> String {
    use quote::ToTokens as _;
    ty.to_token_stream().to_string()
}
