use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::sync::Mutex;
use tower_lsp::jsonrpc::{self, Result};
use tower_lsp::lsp_types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoTypeDefinitionParams,
    GotoTypeDefinitionResponse, WillSaveWaitUntil,
};
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(Debug)]
struct Backend {
    client: Client,
}

#[derive(Debug)]
pub enum Potato {
    Lol,
}

#[derive(Default)]
pub struct State {
    pub init: InitializeParams,

    pub symbols: Vec<(String, String, Option<String>, SymbolKind, Location)>,

    pub symbol_lookup: HashMap<Url, Vec<usize>>,
    pub reverse: HashMap<String, Vec<usize>>,
}

pub fn state_initialize(state: &mut State) {
    let folder = state
        .init
        .workspace_folders
        .as_ref()
        .unwrap()
        .first()
        .unwrap();

    let workspace_folder = folder.uri.to_file_path().unwrap();

    let mut to_do = vec![workspace_folder.clone()];
    let mut files_to_check = vec![];

    let forbidden = ["target", ".git", "backups", "entities", "scenes", "tests", "stdarch", "backtrace"];

    let res = std::process::Command::new("rustup")
        .args(["which", "rustc"])
        .envs(std::env::vars())
        .current_dir(&workspace_folder)
        .output();

    if let Ok(it) = res {
        let it = str::from_utf8(&it.stdout).unwrap();
        let mut path = std::path::PathBuf::from(it);

        path.pop();
        path.pop();
        to_do.push(
            path.join("lib")
                .join("rustlib")
                .join("src")
                .join("rust")
                .join("library"),
        );
    }

    if false {
        /*
            TODO: also get symbols from all deps
        */
        let res = std::process::Command::new("cargo")
            .args(["metadata", "--format-version=1"])
            .current_dir(&workspace_folder)
            .envs(std::env::vars())
            .output();
        if let Ok(it) = res {
            let it = str::from_utf8(&it.stdout).unwrap();
            for it in it.split("manifest_path") {
                if let Some((rest, _rest)) = it.split_once("Cargo.toml") {
                    if let Some((_, rest)) = rest.split_once(":\"") {
                        let mut rest = rest.trim();
                        if let Some((it, _rest)) = rest.rsplit_once('"') {
                            rest = it;
                        }
                        let path = std::path::PathBuf::from(rest);
                        // log!("[dep path] {path:?}\n");
                        to_do.push(path);
                    }
                }
            }
        }
    }

    // let mut visited = HashSet::new();
    'to_do: while let Some(current) = to_do.pop() {
        // let mut current = current;
        // if let Ok(c) = current.canonicalize() {
        //     current = c;
        // }
        // if !visited.insert(current.clone()) {
        //     continue;
        // }

        for it in forbidden {
            if current.ends_with(it) {
                continue 'to_do;
            }
        }

        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };

        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let p = entry.path();
            if p.is_file() {
                if p.extension().is_some_and(|e| e == "rs") {
                    if p.ends_with("tests.rs") {
                        continue;
                    }

                    files_to_check.push(current.join(p));
                }
            } else {
                to_do.push(p);
            }
        }
    }

    // log!("[to check] {files_to_check:#?}\n");

    let symbols = &mut state.symbols;
    let lookup = &mut state.symbol_lookup;
    let reverse = &mut state.reverse;

    let mut buf = String::new();
    let mut last_enum_or_struct_name = String::new();

    for path in files_to_check {
        last_enum_or_struct_name.clear();
        buf.clear();

        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let url = Url::from_file_path(path.clone()).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let entry = lookup.entry(url.clone()).or_default();

        let start = symbols.len();

        let mut line_num = 0;
        'next_line: loop {
            line_num += 1;
            buf.clear();
            let Ok(num) = reader.read_line(&mut buf) else {
                break;
            };
            if num == 0 {
                break;
            }

            let line = buf.trim();

            let bases = [
                (SymbolKind::STRUCT, "struct "),
                (SymbolKind::ENUM, "enum "),
                (SymbolKind::FUNCTION, "fn "),
                (SymbolKind::CONSTANT, "const "),
                (SymbolKind::TYPE_PARAMETER, "type "),
                (SymbolKind::MODULE, "mod "),
                (SymbolKind::VARIABLE, "static "),
                (SymbolKind::FUNCTION, "macro_rules! "),
            ];

            for (kind, base) in bases {
                if line.starts_with(base) || (line.starts_with("pub ") && line.contains(base)) {
                    if let Some((_, rest)) = line.split_once(base) {
                        let mut name = String::new();
                        for char in rest.chars() {
                            if char.is_alphanumeric() || char == '_' || char == '-' {
                                name.push(char);
                                continue;
                            }
                            break;
                        }

                        if kind == SymbolKind::STRUCT || kind == SymbolKind::ENUM {
                            last_enum_or_struct_name = name.clone();
                        }

                        let i = symbols.len();
                        symbols.push((
                            name.clone(),
                            name.to_lowercase(),
                            None,
                            kind,
                            Location::new(
                                url.clone(),
                                Range::new(
                                    Position::new(line_num - 1, 0),
                                    Position::new(line_num - 1, 0),
                                ),
                            ),
                        ));
                        entry.push(i);
                    }

                    continue 'next_line;
                }
            }

            if let Some((_start, rest)) = line.split_once("pub ") {
                let mut name = String::new();
                let mut last_was_colon = false;
                for char in rest.chars() {
                    if char.is_alphanumeric() || char == '_' || char == '-' {
                        name.push(char);
                        continue;
                    }
                    if char == ':' {
                        last_was_colon = true;
                    }
                    break;
                }

                if last_was_colon {
                    let i = symbols.len();
                    symbols.push((
                        name.clone(),
                        name.to_lowercase(),
                        (!last_enum_or_struct_name.is_empty())
                            .then(|| last_enum_or_struct_name.clone()),
                        SymbolKind::FIELD,
                        Location::new(
                            url.clone(),
                            Range::new(
                                Position::new(line_num - 1, 0),
                                Position::new(line_num - 1, 0),
                            ),
                        ),
                    ));
                    entry.push(i);
                }
            }
        }

        let end = symbols.len();
        symbols[start..end].sort_by(|a, b| a.4.range.start.cmp(&b.4.range.start));
    }

    for (i, it) in symbols.iter().enumerate() {
        let e = reverse.entry(it.0.clone()).or_default();
        e.push(i);
    }
}

static mut LOG_PATH: Option<std::path::PathBuf> = None;

#[macro_export]
macro_rules! log {
    ($it:literal) => {
        $crate::log!($it,)
    };

    ($it:literal, $($rest:expr)*) => {{
        let log_path = unsafe {
            #[allow(static_mut_refs)]
            LOG_PATH.get_or_insert_with(|| {
                let here = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                let log_path = here.join("log.log");
                log_path
            })
        };

        use ::std::io::Write;

        let mut file = std::fs::File::options()
            .write(true)
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap();

        let _ = write!(file, $it, $($rest)*);
        let _ = file.flush();
    }};
}

static mut STATE: Option<State> = None;
pub fn state() -> &'static mut State {
    #[allow(static_mut_refs)]
    unsafe {
        STATE.get_or_insert_default()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, initialize_params: InitializeParams) -> Result<InitializeResult> {
        let state = state();
        state.init = initialize_params;

        state_initialize(state);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                completion_provider: Some(CompletionOptions::default()),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                rename_provider: Some(OneOf::Left(true)),
                position_encoding: Some(PositionEncodingKind::UTF8),

                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let _ = params;
        Ok(None)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let _ = params;
        Ok(None)
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let _ = params;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let _ = params;
    }

    async fn will_save(&self, params: WillSaveTextDocumentParams) {
        let _ = params;
    }

    async fn will_save_wait_until(
        &self,
        params: WillSaveTextDocumentParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let _ = params;
        Ok(None)
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let _ = params;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let _ = params;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let state = state();
        let uri = params.text_document_position.text_document.uri;
        Ok(match state.symbol_lookup.get(&uri) {
            None => None,
            Some(it) => Some(CompletionResponse::Array(
                it.iter()
                    .copied()
                    .map(|i| {
                        let name = state.symbols[i].0.clone();
                        CompletionItem::new_simple(name, "".to_string())
                    })
                    .collect::<Vec<_>>(),
            )),
        })
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let _ = params;

        let state = state();
        match state.symbol_lookup.get(&params.text_document.uri) {
            Some(them) => Ok(Some(DocumentSymbolResponse::Flat(
                them.iter()
                    .copied()
                    .map(|index| {
                        let (name, name_lc, container, kind, location) = &state.symbols[index];
                        SymbolInformation {
                            name: name.clone(),
                            kind: *kind,
                            tags: None,
                            deprecated: None,
                            location: location.clone(),
                            container_name: container.clone(),
                        }
                    })
                    .collect(),
            ))),
            None => Ok(None),
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let _ = params;
        let state = state();

        let ql = params.query.to_lowercase();

        use rayon::prelude::*;

        let mut symbols = state
            .symbols
            .par_iter()
            .filter_map(|(name, name_lc, container, kind, location)| {
                (params.query.is_empty()
                    || name.starts_with(&params.query)
                    || name.contains(&params.query)
                    || name_lc.contains(&ql)
                )
                .then(|| SymbolInformation {
                    name: name.clone(),
                    kind: *kind,
                    tags: None,
                    deprecated: None,
                    location: location.clone(),
                    container_name: container.clone(),
                })
            })
            .collect::<Vec<_>>();

        let folder = state
            .init
            .workspace_folders
            .as_ref()
            .unwrap()
            .first()
            .unwrap();

        let workspace_folder = folder.uri.to_file_path().unwrap();

        if !ql.is_empty() {
            symbols.sort_by_key(|it| {
                let mut score = 0_u16;
                if !it.location.uri.to_file_path().unwrap().starts_with(&workspace_folder) {
                    score += 9999;
                }
                let dist = it.name.len().saturating_sub(params.query.len());
                score += dist as u16;
                score
            });
        }

        Ok(Some(symbols))
    }

    async fn symbol_resolve(&self, params: WorkspaceSymbol) -> Result<WorkspaceSymbol> {
        let _ = params;
        Ok(params)
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        Ok(None)
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        Ok(None)
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend { client });
    Server::new(stdin, stdout, socket).serve(service).await;
}
