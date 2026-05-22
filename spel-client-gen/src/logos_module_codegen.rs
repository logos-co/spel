//! Qt/QML Logos Basecamp module scaffold generation from SPEL IDL.
//!
//! Generates: XyzBackend.h/.cpp, XyzPlugin.h/.cpp, src/main.cpp,
//!            qml/Main.qml, module.yaml, manifest.json

use spel_framework_core::idl::*;
use std::collections::HashSet;
use crate::util::*;

pub struct LogosModuleOutput {
    pub backend_h: String,
    pub backend_cpp: String,
    pub plugin_h: String,
    pub plugin_cpp: String,
    pub main_cpp: String,
    pub main_qml: String,
    pub module_yaml: String,
    pub manifest_json: String,
    pub cmake_lists: String,
}

/// `module_name` overrides the name derived from the IDL (e.g. from --module-name).
pub fn generate_logos_module(
    idl: &SpelIdl,
    module_name: Option<&str>,
    ffi_lib_path: Option<&str>,
) -> Result<LogosModuleOutput, String> {
    // effective_prog is the module identity used for file/class/env-var names.
    // prog is the raw IDL snake_case name, used only for FFI function names.
    let effective_prog = module_name
        .map(|n| snake_case(n))
        .unwrap_or_else(|| snake_case(&idl.name));
    let class = pascal_case(&effective_prog);
    let prog = snake_case(&idl.name); // FFI symbol prefix (from IDL, unchanged)
    // Strip trailing _program/_contract before building the env-var prefix so
    // "multisig_program" → "MULTISIG" not "MULTISIG_PROGRAM" → doubled suffix.
    let env_base = effective_prog
        .trim_end_matches("_program")
        .trim_end_matches("_contract")
        .to_uppercase();

    let fetches = fetch_eligible_accounts(idl);

    Ok(LogosModuleOutput {
        backend_h: gen_backend_h(idl, &class, &prog, &fetches, &env_base),
        backend_cpp: gen_backend_cpp(idl, &class, &prog, &fetches, &env_base, &effective_prog),
        plugin_h: gen_plugin_h(&class),
        plugin_cpp: gen_plugin_cpp(&class, &effective_prog),
        main_cpp: gen_main_cpp(&class, &effective_prog),
        main_qml: gen_main_qml(idl, &fetches, &effective_prog),
        module_yaml: gen_module_yaml(idl, &effective_prog, &class),
        manifest_json: gen_manifest_json(idl, &effective_prog),
        cmake_lists: gen_cmake_lists(&class, &effective_prog, ffi_lib_path),
    })
}

// ── Type helpers ──────────────────────────────────────────────────────────────

fn qt_type(ty: &IdlType) -> (String, bool) {
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "u8" | "u16" | "u32" => ("quint32".into(), false),
            // u64/i64 use QString so the full range survives the JS → C++ boundary
            // without precision loss (JS numbers are IEEE-754 doubles, limited to 2^53).
            "u64" | "i64" => ("QString".into(), true),
            "i8" | "i16" | "i32" => ("qint32".into(), false),
            "bool" => ("bool".into(), false),
            _ => ("QString".into(), true),
        },
        IdlType::Vec { vec } => match vec.as_ref() {
            IdlType::Primitive(p)
                if matches!(
                    p.as_str(),
                    "string" | "String" | "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]"
                ) =>
            {
                ("QStringList".into(), true)
            }
            _ => ("QVariantList".into(), true),
        },
        // Option<T> is always QVariant so the C++ layer can detect null (= unchecked).
        IdlType::Option { .. } => ("QVariant".into(), true),
        IdlType::Defined { .. } => ("QVariantMap".into(), true),
        IdlType::Array { array: (elem, _) } => match elem.as_ref() {
            IdlType::Primitive(p) if p == "u8" => ("QString".into(), true),
            _ => ("QVariantList".into(), true),
        },
    }
}

/// If `ty` (possibly wrapped in Option) resolves to a known enum in `idl.types`,
/// return the variant names.  Used to decide whether to emit a ComboBox.
fn enum_variants<'a>(ty: &IdlType, idl: &'a SpelIdl) -> Option<Vec<&'a str>> {
    let inner = match ty {
        IdlType::Option { option } => option.as_ref(),
        other => other,
    };
    if let IdlType::Defined { defined } = inner {
        if let Some(def) = idl.types.iter().find(|t| &t.name == defined && t.kind == "enum") {
            if !def.variants.is_empty() {
                return Some(def.variants.iter().map(|v| v.name.as_str()).collect());
            }
        }
    }
    None
}

/// Human-readable placeholder hint for an IDL field type.
fn type_placeholder(ty: &IdlType) -> &'static str {
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "account_id" | "AccountId" => "base58 or 0x… hex",
            "[u8; 32]" | "[u8;32]" => "base58 or 0x… hex",
            "u8" | "u16" | "u32" | "u64" | "u128" => "integer (≥ 0)",
            "i8" | "i16" | "i32" | "i64" | "i128" => "integer",
            "bool" | "string" | "String" => "",
            _ => "value",
        },
        IdlType::Array { array: (elem, 32) }
            if matches!(elem.as_ref(), IdlType::Primitive(p) if p == "u8") =>
        {
            "base58 or 0x… hex"
        }
        IdlType::Option { option } => type_placeholder(option),
        _ => "value",
    }
}

/// IntValidator bounds for small integer types; None for larger types.
fn validator_str(ty: &IdlType) -> Option<&'static str> {
    if let IdlType::Primitive(p) = ty {
        match p.as_str() {
            "u8"  => Some("IntValidator { bottom: 0; top: 255 }"),
            "u16" => Some("IntValidator { bottom: 0; top: 65535 }"),
            "u32" => Some("IntValidator { bottom: 0; top: 2147483647 }"),
            "i8"  => Some("IntValidator { bottom: -128; top: 127 }"),
            "i16" => Some("IntValidator { bottom: -32768; top: 32767 }"),
            "i32" => Some("IntValidator { bottom: -2147483648; top: 2147483647 }"),
            _ => None,
        }
    } else {
        None
    }
}

/// inputMethodHints for large integer types that can't use IntValidator.
fn input_hints_str(ty: &IdlType) -> Option<&'static str> {
    if let IdlType::Primitive(p) = ty {
        match p.as_str() {
            "u64" | "u128" => Some("Qt.ImhDigitsOnly"),
            "i64" | "i128" => Some("Qt.ImhFormattedNumbersOnly"),
            _ => None,
        }
    } else {
        None
    }
}

fn is_list_type(ty: &IdlType) -> bool {
    matches!(ty, IdlType::Vec { .. })
}

fn qt_param_decl(ty: &IdlType, name: &str) -> String {
    let (t, is_ref) = qt_type(ty);
    if is_ref {
        format!("const {}& {}", t, name)
    } else {
        format!("{} {}", t, name)
    }
}

fn camel_case(s: &str) -> String {
    let p = pascal_case(s);
    if p.is_empty() {
        return p;
    }
    let mut chars = p.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_lowercase().to_string() + chars.as_str(),
    }
}

fn title_case(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_bool_type(ty: &IdlType) -> bool {
    matches!(ty, IdlType::Primitive(p) if p == "bool")
}

// ── Fetch-eligible account analysis ──────────────────────────────────────────

struct FetchAccount {
    acc_name: String,
    seed_params: Vec<(String, IdlType)>,
}

fn fetch_eligible_accounts(idl: &SpelIdl) -> Vec<FetchAccount> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for ix in &idl.instructions {
        for acc in &ix.accounts {
            let pda = match &acc.pda {
                Some(p) => p,
                None => continue,
            };
            let acc_name = snake_case(&acc.name);
            if !seen.insert(acc_name.clone()) {
                continue;
            }

            let seed_params: Vec<(String, IdlType)> = pda
                .seeds
                .iter()
                .filter_map(|seed| {
                    if let IdlSeed::Arg { path } = seed {
                        ix.args
                            .iter()
                            .find(|a| &a.name == path)
                            .map(|a| (a.name.clone(), a.type_.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            result.push(FetchAccount { acc_name, seed_params });
        }
    }
    result
}

// ── Instruction param analysis ────────────────────────────────────────────────

struct InstrParam {
    qt_name: String,
    idl_key: String,
    kind: ParamKind,
}

enum ParamKind {
    /// Non-PDA signer account — const QString& param.
    /// Signer accounts are exposed so the caller can choose which wallet key
    /// to use; the FFI layer resolves signing internally.
    Account,
    Arg(IdlType),
}

fn instruction_params(ix: &IdlInstruction) -> Vec<InstrParam> {
    let mut params = Vec::new();
    for acc in &ix.accounts {
        if acc.signer && acc.pda.is_none() && !acc.rest {
            params.push(InstrParam {
                qt_name: format!("{}Id", camel_case(&acc.name)),
                idl_key: acc.name.clone(),
                kind: ParamKind::Account,
            });
        }
    }
    for arg in &ix.args {
        params.push(InstrParam {
            qt_name: camel_case(&arg.name),
            idl_key: arg.name.clone(),
            kind: ParamKind::Arg(arg.type_.clone()),
        });
    }
    params
}

fn param_cpp_decl(p: &InstrParam) -> String {
    match &p.kind {
        ParamKind::Account => format!("const QString& {}", p.qt_name),
        ParamKind::Arg(ty) => qt_param_decl(ty, &p.qt_name),
    }
}

/// Lines of C++ to add one arg to a QJsonObject named `args`.
fn arg_to_json_lines(ty: &IdlType, qt_name: &str, json_key: &str) -> Vec<String> {
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "u8" | "u16" | "u32" | "i8" | "i16" | "i32" => vec![
                format!("    args[\"{json_key}\"] = static_cast<int>({qt_name});"),
            ],
            // u64/i64 are now QString — pass the raw string so the Rust FFI can
            // parse the full range without loss through JS or QJsonValue (doubles).
            "u64" | "i64" => vec![
                format!("    args[\"{json_key}\"] = {qt_name};"),
            ],
            _ => vec![format!("    args[\"{json_key}\"] = {qt_name};")],
        },
        // Option<T> is QVariant; only add the key if the QML checkbox was checked.
        IdlType::Option { .. } => vec![
            format!("    if ({qt_name}.isValid() && !{qt_name}.isNull()) {{"),
            format!("        args[\"{json_key}\"] = QJsonValue::fromVariant({qt_name});"),
            "    }".to_string(),
        ],
        IdlType::Vec { vec } => match vec.as_ref() {
            // QStringList: elements are already QString — append directly.
            IdlType::Primitive(p)
                if matches!(
                    p.as_str(),
                    "string" | "String" | "account_id" | "AccountId" | "[u8; 32]" | "[u8;32]"
                ) =>
            {
                vec![
                    "    {".to_string(),
                    "        QJsonArray _arr;".to_string(),
                    format!("        for (const QString& _s : {qt_name}) _arr.append(_s);"),
                    format!("        args[\"{json_key}\"] = _arr;"),
                    "    }".to_string(),
                ]
            }
            // QVariantList: convert each element via QJsonValue::fromVariant.
            _ => vec![
                "    {".to_string(),
                "        QJsonArray _arr;".to_string(),
                format!("        for (const QVariant& _v : {qt_name}) _arr.append(QJsonValue::fromVariant(_v));"),
                format!("        args[\"{json_key}\"] = _arr;"),
                "    }".to_string(),
            ],
        },
        _ => vec![format!("    args[\"{json_key}\"] = {qt_name};")],
    }
}

fn param_to_json_lines(p: &InstrParam) -> Vec<String> {
    match &p.kind {
        ParamKind::Account => vec![format!("    args[\"{}\"] = {};", p.idl_key, p.qt_name)],
        ParamKind::Arg(ty) => arg_to_json_lines(ty, &p.qt_name, &p.idl_key),
    }
}

/// Inner QML JS expression for a given IDL type (no Option wrapping).
fn qml_type_expr(ty: &IdlType, field_id: &str, idl: &SpelIdl) -> String {
    // Enum Defined type → ComboBox.currentText
    if enum_variants(ty, idl).is_some() {
        return format!("{field_id}.currentText");
    }
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "bool" => format!("{field_id}.checked"),
            // u64/i64: send raw text string so the full range passes without JS precision loss.
            "u64" | "i64" | "u128" | "i128" => format!("{field_id}.text"),
            "u8" | "u16" | "u32" | "i8" | "i16" | "i32" => format!("parseInt({field_id}.text)"),
            _ => format!("{field_id}.text"),
        },
        IdlType::Vec { .. } => {
            format!("{field_id}.text.split(\"\\n\").map(function(s){{ return s.trim() }}).filter(function(s){{ return s.length > 0 }})")
        }
        IdlType::Option { option } => {
            let inner = if enum_variants(option, idl).is_some() {
                format!("{field_id}.currentText")
            } else {
                qml_type_expr(option, field_id, idl)
            };
            format!("{field_id}_enabled.checked ? ({inner}) : null")
        }
        _ => format!("{field_id}.text"),
    }
}

/// QML JS expression to extract a field value with appropriate type conversion.
fn qml_field_expr(kind: &ParamKind, field_id: &str, idl: &SpelIdl) -> String {
    match kind {
        ParamKind::Account => format!("{field_id}.text"),
        ParamKind::Arg(ty) => qml_type_expr(ty, field_id, idl),
    }
}

// ── Backend.h ─────────────────────────────────────────────────────────────────

fn gen_backend_h(
    idl: &SpelIdl,
    class: &str,
    _prog: &str,
    fetches: &[FetchAccount],
    env_base: &str,
) -> String {
    let mut o = String::new();
    let backend = format!("{class}Backend");
    let has_no_arg_fetches = fetches.iter().any(|f| f.seed_params.is_empty());

    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str("#pragma once\n\n");
    o.push_str("#include <functional>\n");
    o.push_str("#include <QFutureWatcher>\n");
    o.push_str("#include <QJsonArray>\n");
    o.push_str("#include <QJsonObject>\n");
    o.push_str("#include <QObject>\n");
    o.push_str("#include <QString>\n");
    o.push_str("#include <QStringList>\n");
    o.push_str("#include <QTimer>\n");
    o.push_str("#include <QVariantList>\n");
    o.push_str("#include <QVariantMap>\n");
    o.push_str("\nclass LogosAPI;\n\n");
    o.push_str(&format!("class {backend} : public QObject {{\n"));
    o.push_str("    Q_OBJECT\n\n");

    if !fetches.is_empty() {
        o.push_str("    // ── Fetched state ─────────────────────────────────────────────────────\n");
        for f in fetches {
            let p = camel_case(&f.acc_name);
            o.push_str(&format!(
                "    Q_PROPERTY(QVariantMap {p} READ {p} NOTIFY {p}Changed)\n"
            ));
        }
        o.push('\n');
    }

    o.push_str("    // ── Async status ──────────────────────────────────────────────────────\n");
    o.push_str("    Q_PROPERTY(bool       busy       READ busy       NOTIFY busyChanged)\n");
    o.push_str("    Q_PROPERTY(QString    lastError  READ lastError  NOTIFY lastErrorChanged)\n");
    o.push_str("    Q_PROPERTY(QString    lastTxHash READ lastTxHash NOTIFY lastTxHashChanged)\n");
    o.push_str("    Q_PROPERTY(QVariantMap lastResult READ lastResult NOTIFY lastResultChanged)\n\n");
    o.push_str("    // ── Configuration ────────────────────────────────────────────────────\n");
    o.push_str("    Q_PROPERTY(QString walletPath   READ walletPath   WRITE setWalletPath   NOTIFY walletPathChanged)\n");
    o.push_str("    Q_PROPERTY(QString sequencerUrl READ sequencerUrl WRITE setSequencerUrl NOTIFY sequencerUrlChanged)\n");
    o.push_str("    Q_PROPERTY(QString programIdHex READ programIdHex WRITE setProgramIdHex NOTIFY programIdHexChanged)\n\n");
    o.push_str("    // ── Wallet state ─────────────────────────────────────────────────────\n");
    o.push_str("    Q_PROPERTY(QString     connectionStatus  READ connectionStatus  NOTIFY connectionStatusChanged)\n");
    o.push_str("    Q_PROPERTY(QVariantList walletAccounts    READ walletAccounts    NOTIFY walletAccountsChanged)\n");
    o.push_str("    Q_PROPERTY(QVariantMap walletAccountInfo READ walletAccountInfo NOTIFY walletAccountInfoChanged)\n");
    o.push_str("    Q_PROPERTY(QVariantMap walletDecodedAccount READ walletDecodedAccount NOTIFY walletDecodedAccountChanged)\n\n");

    o.push_str("public:\n");
    o.push_str(&format!(
        "    explicit {backend}(LogosAPI* api, QObject* parent = nullptr);\n"
    ));
    o.push_str(&format!("    ~{backend}() override;\n\n"));

    for f in fetches {
        let p = camel_case(&f.acc_name);
        o.push_str(&format!(
            "    QVariantMap {p}() const {{ return m_{p}; }}\n"
        ));
    }
    if !fetches.is_empty() {
        o.push('\n');
    }

    o.push_str("    bool       busy()       const { return m_busy; }\n");
    o.push_str("    QString    lastError()  const { return m_lastError; }\n");
    o.push_str("    QString    lastTxHash() const { return m_lastTxHash; }\n");
    o.push_str("    QVariantMap lastResult() const { return m_lastResult; }\n\n");

    o.push_str("    QString     connectionStatus()  const { return m_connectionStatus; }\n");
    o.push_str("    QVariantList walletAccounts()    const { return m_walletAccounts; }\n");
    o.push_str("    QVariantMap walletAccountInfo() const { return m_walletAccountInfo; }\n");
    o.push_str("    QVariantMap walletDecodedAccount() const { return m_walletDecodedAccount; }\n\n");

    o.push_str("    QString walletPath()   const { return m_walletPath; }\n");
    o.push_str("    QString sequencerUrl() const { return m_sequencerUrl; }\n");
    o.push_str("    QString programIdHex() const { return m_programIdHex; }\n");
    o.push_str("    Q_INVOKABLE void setWalletPath(const QString& v);\n");
    o.push_str("    Q_INVOKABLE void setSequencerUrl(const QString& v);\n");
    o.push_str("    Q_INVOKABLE void setProgramIdHex(const QString& v);\n\n");

    o.push_str("    // ── Instructions ──────────────────────────────────────────────────────\n");
    for ix in &idl.instructions {
        let params = instruction_params(ix);
        let ps = params
            .iter()
            .map(param_cpp_decl)
            .collect::<Vec<_>>()
            .join(", ");
        o.push_str(&format!(
            "    Q_INVOKABLE void {}({ps});\n",
            camel_case(&ix.name)
        ));
    }
    o.push('\n');

    if !fetches.is_empty() {
        o.push_str("    // ── Fetch ─────────────────────────────────────────────────────────────\n");
        for f in fetches {
            let method = format!("fetch{}", pascal_case(&f.acc_name));
            let ps = f
                .seed_params
                .iter()
                .map(|(n, ty)| qt_param_decl(ty, &camel_case(n)))
                .collect::<Vec<_>>()
                .join(", ");
            o.push_str(&format!("    Q_INVOKABLE void {method}({ps});\n"));
        }
        o.push('\n');
    }

    o.push_str("    // ── Wallet ────────────────────────────────────────────────────────────\n");
    o.push_str("    Q_INVOKABLE void checkConnection();\n");
    o.push_str("    Q_INVOKABLE void listAccounts();\n");
    o.push_str("    Q_INVOKABLE void createAccount(const QString& label);\n");
    o.push_str("    Q_INVOKABLE void inspectAccount(const QString& accountId);\n");
    o.push_str("    Q_INVOKABLE void decodeAccount(const QString& accountId);\n");
    o.push_str("    Q_INVOKABLE QStringList fieldHistory(const QString& key) const;\n");
    o.push_str("    Q_INVOKABLE void        saveHistory(const QString& key, const QString& value);\n\n");
    o.push_str("signals:\n");
    for f in fetches {
        let p = camel_case(&f.acc_name);
        o.push_str(&format!("    void {p}Changed();\n"));
    }
    o.push_str("    void busyChanged();\n");
    o.push_str("    void lastErrorChanged();\n");
    o.push_str("    void lastTxHashChanged();\n");
    o.push_str("    void lastResultChanged();\n");
    o.push_str("    void operationSuccess(const QString& operation, const QString& txHash);\n");
    o.push_str("    void operationError(const QString& operation, const QString& error);\n");
    o.push_str("    void walletPathChanged();\n");
    o.push_str("    void sequencerUrlChanged();\n");
    o.push_str("    void programIdHexChanged();\n");
    o.push_str("    void connectionStatusChanged();\n");
    o.push_str("    void walletAccountsChanged();\n");
    o.push_str("    void walletAccountInfoChanged();\n");
    o.push_str("    void walletDecodedAccountChanged();\n\n");

    o.push_str("private:\n");
    if has_no_arg_fetches {
        o.push_str("    Q_SLOT void autoRefresh();\n\n");
    }
    o.push_str("    using FfiFn = char* (*)(const char*);\n\n");
    o.push_str(
        "    void        dispatchFfi(const QString& operation, std::function<QString()> fn);\n",
    );
    o.push_str(
        "    void        handleFfiResult(const QString& operation, const QString& result);\n",
    );
    o.push_str("    QString     callFfi(FfiFn fn, const QJsonObject& args);\n");
    o.push_str("    QJsonObject baseArgs() const;\n\n");
    o.push_str("    QString m_walletPath;\n");
    o.push_str("    QString m_sequencerUrl;\n");
    o.push_str("    QString m_programIdHex;\n\n");
    for f in fetches {
        let p = camel_case(&f.acc_name);
        o.push_str(&format!("    QVariantMap m_{p};\n"));
    }
    if !fetches.is_empty() {
        o.push('\n');
    }
    o.push_str("    bool       m_busy      = false;\n");
    o.push_str("    QString    m_lastError;\n");
    o.push_str("    QString    m_lastTxHash;\n");
    o.push_str("    QVariantMap m_lastResult;\n");
    o.push_str("    QString     m_connectionStatus;\n");
    o.push_str("    QVariantList m_walletAccounts;\n");
    o.push_str("    QVariantMap m_walletAccountInfo;\n");
    o.push_str("    QVariantMap m_walletDecodedAccount;\n");
    o.push_str("};\n");

    // Remind dev of the expected env var
    o.push_str(&format!(
        "\n// Expected environment variable: {env_base}_PROGRAM_ID\n"
    ));

    o
}

// ── Backend.cpp ───────────────────────────────────────────────────────────────

fn gen_backend_cpp(
    idl: &SpelIdl,
    class: &str,
    prog: &str,
    fetches: &[FetchAccount],
    env_base: &str,
    effective_prog: &str,
) -> String {
    let mut o = String::new();
    let backend = format!("{class}Backend");

    // No-arg fetches for autoRefresh
    let no_arg_fetches: Vec<&FetchAccount> =
        fetches.iter().filter(|f| f.seed_params.is_empty()).collect();
    let has_no_arg_fetches = !no_arg_fetches.is_empty();

    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str(&format!("#include \"{backend}.h\"\n\n"));
    o.push_str("#include <QJsonArray>\n");
    o.push_str("#include <QJsonDocument>\n");
    o.push_str("#include <QJsonObject>\n");
    o.push_str("#include <QMetaObject>\n");
    o.push_str("#include <QSettings>\n");
    o.push_str("#include <QThreadPool>\n");
    o.push_str("#include <QTimer>\n");
    o.push_str("#include <QtConcurrent/QtConcurrent>\n\n");

    // extern "C" declarations
    o.push_str("extern \"C\" {\n");
    for ix in &idl.instructions {
        let fn_name = format!("{}_{}", prog, snake_case(&ix.name));
        o.push_str(&format!("    char* {fn_name}(const char* args_json);\n"));
    }
    for f in fetches {
        o.push_str(&format!(
            "    char* {prog}_fetch_{}(const char* args_json);\n",
            f.acc_name
        ));
    }
    o.push_str(&format!("    char* {effective_prog}_program_id();\n"));
    o.push_str(&format!("    void  {prog}_free_string(char* s);\n"));
    o.push_str(&format!("    char* {prog}_check_connection(const char* args_json);\n"));
    o.push_str(&format!("    char* {prog}_inspect_account(const char* args_json);\n"));
    o.push_str(&format!("    char* {prog}_list_accounts(const char* args_json);\n"));
    o.push_str(&format!("    char* {prog}_create_account(const char* args_json);\n"));
    o.push_str(&format!("    char* {prog}_decode_account(const char* args_json);\n"));
    o.push_str("}\n\n");

    // Constructor
    o.push_str("// ── Construction ──────────────────────────────────────────────────────────\n\n");
    o.push_str(&format!("{backend}::{backend}(LogosAPI* /*api*/, QObject* parent)\n"));
    o.push_str("    : QObject(parent)\n{\n");
    o.push_str(&format!("    QSettings s(\"logos-co\", \"{effective_prog}\");\n"));
    o.push_str("    m_walletPath   = s.value(\"walletPath\",   qEnvironmentVariable(\"NSSA_WALLET_HOME_DIR\",  \".scaffold/wallet\")).toString();\n");
    o.push_str("    m_sequencerUrl = s.value(\"sequencerUrl\", qEnvironmentVariable(\"NSSA_SEQUENCER_URL\",   \"http://127.0.0.1:3040\")).toString();\n");
    o.push_str(&format!(
        "    m_programIdHex = s.value(\"programIdHex\", qEnvironmentVariable(\"{env_base}_PROGRAM_ID\")).toString();\n"
    ));
    // Fallback: call the compiled-in FFI constant if still empty (priority 3)
    o.push_str(&format!("    if (m_programIdHex.isEmpty()) {{\n"));
    o.push_str(&format!("        char* raw = {effective_prog}_program_id();\n"));
    o.push_str("        if (raw) {\n");
    o.push_str("            m_programIdHex = QJsonDocument::fromJson(QByteArray(raw))\n");
    o.push_str("                                 .object().value(\"program_id_hex\").toString();\n");
    o.push_str(&format!("            {prog}_free_string(raw);\n"));
    o.push_str("        }\n");
    o.push_str("    }\n");
    o.push_str("    // Pre-populate account list so field pickers have data before wallet page is visited.\n");
    o.push_str("    if (!m_walletPath.isEmpty())\n");
    o.push_str("        QTimer::singleShot(0, this, [this] { listAccounts(); });\n");
    o.push_str("}\n\n");
    o.push_str(&format!("{backend}::~{backend}() = default;\n\n"));

    // Configuration setters (QSettings-backed, priority: QSettings > env var)
    o.push_str("// ── Configuration ────────────────────────────────────────────────────────\n\n");
    for (field, method, key, signal) in [
        ("m_walletPath",   "setWalletPath",   "walletPath",   "walletPathChanged"),
        ("m_sequencerUrl", "setSequencerUrl", "sequencerUrl", "sequencerUrlChanged"),
        ("m_programIdHex", "setProgramIdHex", "programIdHex", "programIdHexChanged"),
    ] {
        o.push_str(&format!("void {backend}::{method}(const QString& v) {{\n"));
        o.push_str(&format!("    if ({field} == v) return;\n"));
        o.push_str(&format!("    {field} = v;\n"));
        o.push_str(&format!("    QSettings(\"logos-co\", \"{effective_prog}\").setValue(\"{key}\", v);\n"));
        o.push_str(&format!("    emit {signal}();\n"));
        if method == "setWalletPath" {
            o.push_str("    if (!v.isEmpty()) listAccounts();\n");
        }
        o.push_str("}\n\n");
    }

    // Helpers
    o.push_str("// ── Helpers ──────────────────────────────────────────────────────────────\n\n");
    o.push_str(&format!("QJsonObject {backend}::baseArgs() const {{\n"));
    o.push_str("    return QJsonObject{\n");
    o.push_str("        {\"wallet_path\",    m_walletPath},\n");
    o.push_str("        {\"sequencer_url\",  m_sequencerUrl},\n");
    o.push_str("        {\"program_id_hex\", m_programIdHex},\n");
    o.push_str("    };\n}\n\n");

    o.push_str(&format!(
        "QString {backend}::callFfi(FfiFn fn, const QJsonObject& args) {{\n"
    ));
    o.push_str("    QByteArray json = QJsonDocument(args).toJson(QJsonDocument::Compact);\n");
    o.push_str("    char* raw = fn(json.constData());\n");
    o.push_str(
        "    if (!raw) return R\"({\"success\":false,\"error\":\"null return from FFI\"})\";\n",
    );
    o.push_str("    QString result = QString::fromUtf8(raw);\n");
    o.push_str(&format!("    {prog}_free_string(raw);\n"));
    o.push_str("    return result;\n}\n\n");

    o.push_str(&format!(
        "void {backend}::dispatchFfi(const QString& operation, std::function<QString()> fn) {{\n"
    ));
    o.push_str("    if (m_busy) return;\n");
    o.push_str("    m_busy = true;\n");
    o.push_str("    emit busyChanged();\n\n");
    o.push_str("    auto* watcher = new QFutureWatcher<QString>(this);\n");
    o.push_str("    connect(watcher, &QFutureWatcher<QString>::finished, this, [this, watcher, operation]() {\n");
    o.push_str("        handleFfiResult(operation, watcher->result());\n");
    o.push_str("        watcher->deleteLater();\n");
    o.push_str("        m_busy = false;\n");
    o.push_str("        emit busyChanged();\n");
    o.push_str("    });\n");
    o.push_str("    watcher->setFuture(QtConcurrent::run(fn));\n}\n\n");

    o.push_str(&format!(
        "void {backend}::handleFfiResult(const QString& operation, const QString& result) {{\n"
    ));
    o.push_str("    QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n\n");
    o.push_str("    if (!obj.value(\"success\").toBool()) {\n");
    o.push_str("        m_lastError = obj.value(\"error\").toString(result);\n");
    o.push_str("        emit lastErrorChanged();\n");
    o.push_str("        emit operationError(operation, m_lastError);\n");
    o.push_str("        return;\n    }\n\n");
    o.push_str("    m_lastError.clear();\n");
    o.push_str("    emit lastErrorChanged();\n\n");
    o.push_str("    m_lastResult = obj.toVariantMap();\n");
    o.push_str("    emit lastResultChanged();\n\n");
    o.push_str("    if (obj.contains(\"tx_hash\")) {\n");
    o.push_str("        m_lastTxHash = obj.value(\"tx_hash\").toString();\n");
    o.push_str("        emit lastTxHashChanged();\n");
    o.push_str("        emit operationSuccess(operation, m_lastTxHash);\n");
    if has_no_arg_fetches {
        o.push_str("        QTimer::singleShot(1200, this, &");
        o.push_str(&backend);
        o.push_str("::autoRefresh);\n");
    }
    o.push_str("    } else {\n");
    o.push_str("        emit operationSuccess(operation, QString());\n");
    o.push_str("    }\n}\n\n");

    // autoRefresh
    if has_no_arg_fetches {
        o.push_str(&format!("void {backend}::autoRefresh() {{\n"));
        for f in &no_arg_fetches {
            let method = format!("fetch{}", pascal_case(&f.acc_name));
            o.push_str(&format!("    {method}();\n"));
        }
        o.push_str("}\n\n");
    }

    // Instructions
    o.push_str("// ── Instructions ─────────────────────────────────────────────────────────\n\n");
    for ix in &idl.instructions {
        let params = instruction_params(ix);
        let ps = params
            .iter()
            .map(param_cpp_decl)
            .collect::<Vec<_>>()
            .join(", ");
        let method = camel_case(&ix.name);
        let fn_name = format!("{}_{}", prog, snake_case(&ix.name));

        o.push_str(&format!("void {backend}::{method}({ps}) {{\n"));
        o.push_str("    QJsonObject args = baseArgs();\n");
        for p in &params {
            for line in param_to_json_lines(p) {
                o.push_str(&line);
                o.push('\n');
            }
        }
        o.push_str(&format!(
            "    dispatchFfi(\"{}\", [this, args]() {{\n",
            ix.name
        ));
        o.push_str(&format!("        return callFfi({fn_name}, args);\n"));
        o.push_str("    });\n}\n\n");
    }

    // Fetch methods
    if !fetches.is_empty() {
        o.push_str("// ── Fetch ────────────────────────────────────────────────────────────────\n\n");
        for f in fetches {
            let method = format!("fetch{}", pascal_case(&f.acc_name));
            let prop = camel_case(&f.acc_name);
            let fn_name = format!("{prog}_fetch_{}", f.acc_name);
            let ps = f
                .seed_params
                .iter()
                .map(|(n, ty)| qt_param_decl(ty, &camel_case(n)))
                .collect::<Vec<_>>()
                .join(", ");

            o.push_str(&format!("void {backend}::{method}({ps}) {{\n"));
            o.push_str("    QJsonObject args = baseArgs();\n");
            for (name, ty) in &f.seed_params {
                let qn = camel_case(name);
                for line in arg_to_json_lines(ty, &qn, name) {
                    o.push_str(&line);
                    o.push('\n');
                }
            }
            o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
            o.push_str(&format!(
                "        QString result = callFfi({fn_name}, args);\n"
            ));
            o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
            o.push_str(
                "            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n",
            );
            o.push_str("            if (obj.value(\"success\").toBool() && obj.contains(\"state\")) {\n");
            o.push_str(&format!(
                "                m_{prop} = obj.value(\"state\").toObject().toVariantMap();\n"
            ));
            o.push_str(&format!("                emit {prop}Changed();\n"));
            o.push_str("            }\n");
            o.push_str("        }, Qt::QueuedConnection);\n");
            o.push_str("    });\n}\n\n");
        }
    }

    // Wallet methods
    o.push_str("// ── Wallet ──────────────────────────────────────────────────────────────\n\n");
    o.push_str(&format!("void {backend}::checkConnection() {{\n"));
    o.push_str("    QJsonObject args = baseArgs();\n");
    o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
    o.push_str(&format!("        QString result = callFfi({prog}_check_connection, args);\n"));
    o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
    o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
    o.push_str("            if (obj.value(\"success\").toBool()) {\n");
    o.push_str("                m_connectionStatus = \"\\u2713 \" + obj.value(\"sequencer_url\").toString();\n");
    o.push_str("            } else {\n");
    o.push_str("                m_connectionStatus = \"\\u2717 \" + obj.value(\"error\").toString(result);\n");
    o.push_str("            }\n");
    o.push_str("            emit connectionStatusChanged();\n");
    o.push_str("        }, Qt::QueuedConnection);\n");
    o.push_str("    });\n}\n\n");

    o.push_str(&format!("void {backend}::listAccounts() {{\n"));
    o.push_str("    QJsonObject args = baseArgs();\n");
    o.push_str("    args[\"program_id_hex\"] = m_programIdHex;\n");
    o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
    o.push_str(&format!("        QString result = callFfi({prog}_list_accounts, args);\n"));
    o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
    o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
    o.push_str("            if (obj.value(\"success\").toBool()) {\n");
    o.push_str("                QVariantList list;\n");
    o.push_str("                for (const QJsonValue& v : obj.value(\"accounts\").toArray()) {\n");
    o.push_str("                    QJsonObject ao = v.toObject();\n");
    o.push_str("                    QVariantMap item;\n");
    o.push_str("                    item[\"id\"]     = ao.value(\"id\").toString();\n");
    o.push_str("                    item[\"label\"]  = ao.value(\"label\").toString();\n");
    o.push_str("                    item[\"path\"]   = ao.value(\"path\").toString();\n");
    o.push_str("                    item[\"status\"] = ao.value(\"status\").toString();\n");
    o.push_str("                    list.append(item);\n");
    o.push_str("                }\n");
    o.push_str("                m_walletAccounts = list;\n");
    o.push_str("                emit walletAccountsChanged();\n");
    o.push_str("            } else {\n");
    o.push_str("                m_lastError = obj.value(\"error\").toString(result);\n");
    o.push_str("                emit lastErrorChanged();\n");
    o.push_str("            }\n");
    o.push_str("        }, Qt::QueuedConnection);\n");
    o.push_str("    });\n}\n\n");

    o.push_str(&format!("void {backend}::createAccount(const QString& label) {{\n"));
    o.push_str("    QJsonObject args = baseArgs();\n");
    o.push_str("    if (!label.isEmpty()) args[\"label\"] = label;\n");
    o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
    o.push_str(&format!("        QString result = callFfi({prog}_create_account, args);\n"));
    o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
    o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
    o.push_str("            if (obj.value(\"success\").toBool()) {\n");
    o.push_str("                QString newId = obj.value(\"account_id\").toString();\n");
    o.push_str("                emit operationSuccess(\"create_account\", newId);\n");
    o.push_str("                listAccounts();\n");
    o.push_str("            } else {\n");
    o.push_str("                m_lastError = obj.value(\"error\").toString(result);\n");
    o.push_str("                emit lastErrorChanged();\n");
    o.push_str("                emit operationError(\"create_account\", m_lastError);\n");
    o.push_str("            }\n");
    o.push_str("        }, Qt::QueuedConnection);\n");
    o.push_str("    });\n}\n\n");

    o.push_str(&format!("void {backend}::inspectAccount(const QString& accountId) {{\n"));
    o.push_str("    QJsonObject args = baseArgs();\n");
    o.push_str("    args[\"account_id\"] = accountId;\n");
    // pass program_id_hex so the FFI can classify owner status
    o.push_str("    args[\"program_id_hex\"] = m_programIdHex;\n");
    o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
    o.push_str(&format!("        QString result = callFfi({prog}_inspect_account, args);\n"));
    o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
    o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
    o.push_str("            if (obj.value(\"success\").toBool()) {\n");
    o.push_str("                QVariantMap info;\n");
    o.push_str("                info[\"status\"]          = obj.value(\"status\").toString();\n");
    o.push_str("                info[\"data_len\"]        = obj.value(\"data_len\").toInt();\n");
    o.push_str("                info[\"data_preview\"]    = obj.value(\"data_preview\").toString();\n");
    o.push_str("                info[\"program_owner\"]   = obj.value(\"program_owner\").toString();\n");
    o.push_str("                info[\"has_signing_key\"] = obj.value(\"has_signing_key\").toBool() ? \"yes\" : \"no\";\n");
    o.push_str("                m_walletAccountInfo = info;\n");
    o.push_str("            } else {\n");
    o.push_str("                QVariantMap info;\n");
    o.push_str("                info[\"error\"] = obj.value(\"error\").toString();\n");
    o.push_str("                m_walletAccountInfo = info;\n");
    o.push_str("            }\n");
    o.push_str("            emit walletAccountInfoChanged();\n");
    o.push_str("        }, Qt::QueuedConnection);\n");
    o.push_str("    });\n}\n\n");

    o.push_str(&format!("void {backend}::decodeAccount(const QString& accountId) {{\n"));
    o.push_str("    QJsonObject args = baseArgs();\n");
    o.push_str("    args[\"account_id\"] = accountId;\n");
    o.push_str("    QThreadPool::globalInstance()->start([this, args]() {\n");
    o.push_str(&format!("        QString result = callFfi({prog}_decode_account, args);\n"));
    o.push_str("        QMetaObject::invokeMethod(this, [this, result]() {\n");
    o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
    o.push_str("            if (obj.value(\"success\").toBool()) {\n");
    o.push_str("                QVariantMap decoded;\n");
    o.push_str("                decoded[\"type\"] = obj.value(\"type\").toString();\n");
    o.push_str("                if (!obj.value(\"fields\").isNull()) {\n");
    o.push_str("                    QJsonObject fields = obj.value(\"fields\").toObject();\n");
    o.push_str("                    QVariantMap fmap;\n");
    o.push_str("                    for (auto it = fields.begin(); it != fields.end(); ++it)\n");
    o.push_str("                        fmap[it.key()] = it.value().toVariant();\n");
    o.push_str("                    decoded[\"fields\"] = fmap;\n");
    o.push_str("                }\n");
    o.push_str("                if (!obj.value(\"raw_hex\").isNull())\n");
    o.push_str("                    decoded[\"raw_hex\"] = obj.value(\"raw_hex\").toString();\n");
    o.push_str("                if (!obj.value(\"status\").isNull())\n");
    o.push_str("                    decoded[\"status\"] = obj.value(\"status\").toString();\n");
    o.push_str("                m_walletDecodedAccount = decoded;\n");
    o.push_str("            } else {\n");
    o.push_str("                m_walletDecodedAccount = {{ {\"error\", obj.value(\"error\").toString()} }};\n");
    o.push_str("            }\n");
    o.push_str("            emit walletDecodedAccountChanged();\n");
    o.push_str("        }, Qt::QueuedConnection);\n");
    o.push_str("    });\n}\n\n");

    // ── Field history ─────────────────────────────────────────────────────────
    o.push_str("// ── Field history ────────────────────────────────────────────────────────\n\n");
    o.push_str(&format!("QStringList {backend}::fieldHistory(const QString& key) const {{\n"));
    o.push_str(&format!("    return QSettings(\"logos-co\", \"{effective_prog}\")\n"));
    o.push_str("               .value(\"history/\" + key, QStringList{}).toStringList();\n");
    o.push_str("}\n\n");
    o.push_str(&format!("void {backend}::saveHistory(const QString& key, const QString& value) {{\n"));
    o.push_str("    if (value.trimmed().isEmpty()) return;\n");
    o.push_str(&format!("    QSettings s(\"logos-co\", \"{effective_prog}\");\n"));
    o.push_str("    QStringList h = s.value(\"history/\" + key, QStringList{}).toStringList();\n");
    o.push_str("    h.removeAll(value);\n");
    o.push_str("    h.prepend(value);\n");
    o.push_str("    if (h.size() > 10) h.resize(10);\n");
    o.push_str("    s.setValue(\"history/\" + key, h);\n");
    o.push_str("}\n\n");

    o
}

// ── Plugin.h ──────────────────────────────────────────────────────────────────

fn gen_plugin_h(class: &str) -> String {
    // Basecamp uses the IComponent interface (createWidget/destroyWidget),
    // NOT QQmlExtensionPlugin::registerTypes.
    format!(
        "// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n\
         #pragma once\n\n\
         #include <QObject>\n\
         #include <QWidget>\n\
         #include <QtPlugin>\n\n\
         class LogosAPI;\n\
         class {class}Backend;\n\n\
         class IComponent {{\n\
         public:\n\
         \tvirtual ~IComponent() = default;\n\
         \tvirtual QWidget* createWidget(LogosAPI* api = nullptr) = 0;\n\
         \tvirtual void     destroyWidget(QWidget* widget) = 0;\n\
         }};\n\
         #define IComponent_iid \"com.logos.component.IComponent\"\n\
         Q_DECLARE_INTERFACE(IComponent, IComponent_iid)\n\n\
         class {class}Plugin : public QObject, public IComponent {{\n\
         \tQ_OBJECT\n\
         \tQ_PLUGIN_METADATA(IID IComponent_iid FILE \"../manifest.json\")\n\
         \tQ_INTERFACES(IComponent)\n\n\
         public:\n\
         \texplicit {class}Plugin(QObject* parent = nullptr);\n\
         \t~{class}Plugin() override;\n\n\
         \tQ_INVOKABLE void initLogos(LogosAPI* api);\n\n\
         \tQWidget* createWidget(LogosAPI* api = nullptr) override;\n\
         \tvoid     destroyWidget(QWidget* widget) override;\n\n\
         private:\n\
         \tLogosAPI*      m_api     = nullptr;\n\
         \t{class}Backend* m_backend = nullptr;\n\
         }};\n"
    )
}

// ── Plugin.cpp ────────────────────────────────────────────────────────────────

fn gen_plugin_cpp(class: &str, effective_prog: &str) -> String {
    // Q_INIT_RESOURCE name must match the qt_add_resources() name in CMakeLists.txt.
    // Convention: <effective_prog>_qml  (snake_case with _qml suffix).
    let res = effective_prog.replace('-', "_");
    format!(
        "// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n\
         #include \"{class}Plugin.h\"\n\
         #include \"{class}Backend.h\"\n\n\
         #include <QQmlContext>\n\
         #include <QQmlEngine>\n\
         #include <QQuickWidget>\n\
         #include <QUrl>\n\
         #include <cstdlib>\n\n\
         {class}Plugin::{class}Plugin(QObject* parent) : QObject(parent) {{}}\n\
         {class}Plugin::~{class}Plugin() = default;\n\n\
         void {class}Plugin::initLogos(LogosAPI* api) {{\n\
         \tm_api = api;\n\
         }}\n\n\
         QWidget* {class}Plugin::createWidget(LogosAPI* api) {{\n\
         \tif (api) m_api = api;\n\
         \tif (!m_backend)\n\
         \t\tm_backend = new {class}Backend(m_api, this);\n\
         \tauto* view = new QQuickWidget();\n\
         \tview->engine()->rootContext()->setContextProperty(\"backend\", m_backend);\n\
         \tview->setResizeMode(QQuickWidget::SizeRootObjectToView);\n\
         \tconst char* qmlPath = std::getenv(\"QML_PATH\");\n\
         \tif (qmlPath) {{\n\
         \t\tview->setSource(QUrl::fromLocalFile(QString::fromUtf8(qmlPath) + \"/Main.qml\"));\n\
         \t}} else {{\n\
         \t\t// Qt does not auto-register embedded resources in dynamically loaded plugins.\n\
         \t\tQ_INIT_RESOURCE({res}_qml); // name must match qt_add_resources() in CMakeLists.txt\n\
         \t\tview->setSource(QUrl(\"qrc:/qml/Main.qml\"));\n\
         \t}}\n\
         \treturn view;\n\
         }}\n\n\
         void {class}Plugin::destroyWidget(QWidget* widget) {{\n\
         \tdelete m_backend;\n\
         \tm_backend = nullptr;\n\
         \tdelete widget;\n\
         }}\n"
    )
}

// ── src/main.cpp ──────────────────────────────────────────────────────────────

fn gen_main_cpp(class: &str, effective_prog: &str) -> String {
    let title = pascal_case(effective_prog).replace('_', " ");
    let env_hint = effective_prog
        .trim_end_matches("_program")
        .trim_end_matches("_contract")
        .to_uppercase();
    format!(
        "// Standalone preview app — loads the QML UI without Basecamp.\n\
         // Build with: cmake -B build && cmake --build build\n\
         // Run with:   {env_hint}_PROGRAM_ID=<hex> ./build/{effective_prog}_app\n\n\
         #include \"{class}Backend.h\"\n\
         #include \"{class}Plugin.h\"\n\n\
         #include <QApplication>\n\
         #include <QQmlContext>\n\
         #include <QQmlEngine>\n\
         #include <QQuickWidget>\n\
         #include <QUrl>\n\
         #include <cstdlib>\n\n\
         int main(int argc, char** argv) {{\n\
         \tQApplication app(argc, argv);\n\
         \tapp.setOrganizationName(\"logos-co\");\n\
         \tapp.setApplicationName(\"{effective_prog}\");\n\n\
         \t{class}Backend backend(nullptr);\n\n\
         \tQQuickWidget view;\n\
         \tview.engine()->rootContext()->setContextProperty(\"backend\", &backend);\n\
         \tview.setResizeMode(QQuickWidget::SizeRootObjectToView);\n\
         \tview.resize(900, 640);\n\n\
         \tconst char* qmlPath = std::getenv(\"QML_PATH\");\n\
         \tif (qmlPath)\n\
         \t\tview.setSource(QUrl::fromLocalFile(QString::fromUtf8(qmlPath) + \"/Main.qml\"));\n\
         \telse\n\
         \t\tview.setSource(QUrl(\"qrc:/qml/Main.qml\"));\n\n\
         \tview.setWindowTitle(\"{title}\");\n\
         \tview.show();\n\
         \treturn app.exec();\n\
         }}\n"
    )
}

// ── Main.qml ─────────────────────────────────────────────────────────────────

fn gen_main_qml(idl: &SpelIdl, fetches: &[FetchAccount], effective_prog: &str) -> String {
    let prog_title = title_case(
        effective_prog
            .trim_end_matches("_program")
            .trim_end_matches("_contract"),
    );

    let mut o = String::new();
    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str("import QtQuick 2.15\n");
    o.push_str("import QtQuick.Controls 2.15\n");
    o.push_str("import QtQuick.Layouts 1.15\n\n");

    o.push_str("Item {\n    id: root\n\n");
    // Page index drives the StackLayout — updated by sidebar nav items.
    o.push_str("    property int currentPageIndex: 0\n\n");

    // Palette
    o.push_str("    // ── Logos palette ────────────────────────────────────────────────\n");
    o.push_str("    readonly property color colBg:      \"#0f1117\"\n");
    o.push_str("    readonly property color colSurface: \"#1a1d27\"\n");
    o.push_str("    readonly property color colSidebar: \"#13151f\"\n");
    o.push_str("    readonly property color colBorder:  \"#2d3148\"\n");
    o.push_str("    readonly property color colPrimary: \"#7c6ef5\"\n");
    o.push_str("    readonly property color colSuccess: \"#3ecf8e\"\n");
    o.push_str("    readonly property color colError:   \"#e05252\"\n");
    o.push_str("    readonly property color colText:    \"#e8e9f0\"\n");
    o.push_str("    readonly property color colMuted:   \"#6b7280\"\n");
    o.push_str("    readonly property int   radius:     12\n\n");

    // Connections
    o.push_str("    Connections {\n");
    o.push_str("        target: backend\n");
    o.push_str("        function onOperationSuccess(operation, txHash) {\n");
    o.push_str("            toast.show(\"\\u2713 \" + operation + (txHash ? \" \\u00b7 \" + txHash.slice(0, 12) + \"\\u2026\" : \"\"), root.colSuccess, 4000)\n");
    o.push_str("        }\n");
    o.push_str("        function onOperationError(operation, error) {\n");
    o.push_str("            toast.show(\"\\u2717 \" + operation + \": \" + error, root.colError, 7000)\n");
    o.push_str("        }\n");
    o.push_str("    }\n\n");

    // Background
    o.push_str("    Rectangle {\n");
    o.push_str("        anchors.fill: parent\n");
    o.push_str("        color: root.colBg\n\n");

    // ── Sidebar ──────────────────────────────────────────────────────────────
    o.push_str("        // ── Sidebar ──────────────────────────────────────────────────────\n");
    o.push_str("        Rectangle {\n");
    o.push_str("            id: sidebar\n");
    o.push_str("            anchors { left: parent.left; top: parent.top; bottom: parent.bottom }\n");
    o.push_str("            width: 200\n");
    o.push_str("            color: root.colSidebar\n\n");
    o.push_str("            ColumnLayout {\n");
    o.push_str("                anchors.fill: parent\n");
    o.push_str("                spacing: 0\n\n");
    // Header row
    o.push_str("                RowLayout {\n");
    o.push_str("                    Layout.fillWidth: true\n");
    o.push_str("                    Layout.preferredHeight: 52\n");
    o.push_str("                    Layout.leftMargin: 16; Layout.rightMargin: 8\n");
    o.push_str(&format!("                    Text {{\n"));
    o.push_str(&format!("                        text: \"{prog_title}\"\n"));
    o.push_str("                        color: root.colText\n");
    o.push_str("                        font.pixelSize: 15; font.bold: true\n");
    o.push_str("                        Layout.fillWidth: true\n");
    o.push_str("                    }\n");
    o.push_str("                    Row {\n");
    o.push_str("                        visible: backend.busy\n");
    o.push_str("                        spacing: 4\n");
    o.push_str("                        Repeater {\n");
    o.push_str("                            model: 3\n");
    o.push_str("                            Rectangle {\n");
    o.push_str("                                width: 5; height: 5; radius: 2.5\n");
    o.push_str("                                color: root.colPrimary\n");
    o.push_str("                                SequentialAnimation on opacity {\n");
    o.push_str("                                    running: backend.busy; loops: Animation.Infinite\n");
    o.push_str("                                    PauseAnimation   { duration: index * 200 }\n");
    o.push_str("                                    NumberAnimation  { to: 1.0; duration: 200 }\n");
    o.push_str("                                    NumberAnimation  { to: 0.25; duration: 200 }\n");
    o.push_str("                                    PauseAnimation   { duration: (2 - index) * 200 }\n");
    o.push_str("                                }\n");
    o.push_str("                            }\n");
    o.push_str("                        }\n");
    o.push_str("                    }\n");
    o.push_str("                }\n\n");
    // Divider
    o.push_str("                Rectangle { Layout.fillWidth: true; height: 1; color: root.colBorder }\n\n");
    // Scrollable nav column with section labels and dividers.
    o.push_str("                ScrollView {\n");
    o.push_str("                    Layout.fillWidth: true\n");
    o.push_str("                    Layout.fillHeight: true\n");
    o.push_str("                    clip: true\n");
    o.push_str("                    contentWidth: sidebar.width\n\n");
    o.push_str("                    Column {\n");
    o.push_str("                        width: sidebar.width\n\n");

    // Helper closure: emit one nav item
    let emit_nav_item = |o: &mut String, label: &str, page_idx: usize| {
        o.push_str("                        ItemDelegate {\n");
        o.push_str("                            width: sidebar.width\n");
        o.push_str("                            height: 40\n");
        o.push_str(&format!("                            onClicked: root.currentPageIndex = {page_idx}\n"));
        o.push_str("                            background: Rectangle {\n");
        o.push_str(&format!("                                color: root.currentPageIndex === {page_idx}\n"));
        o.push_str("                                       ? Qt.rgba(0.49, 0.43, 0.96, 0.15) : \"transparent\"\n");
        o.push_str("                            }\n");
        o.push_str("                            contentItem: Text {\n");
        o.push_str(&format!("                                text: \"{label}\"\n"));
        o.push_str(&format!("                                color: root.currentPageIndex === {page_idx} ? root.colPrimary : root.colText\n"));
        o.push_str("                                font.pixelSize: 13\n");
        o.push_str("                                leftPadding: 16\n");
        o.push_str("                                verticalAlignment: Text.AlignVCenter\n");
        o.push_str("                            }\n");
        o.push_str("                        }\n");
    };
    let emit_section_label = |o: &mut String, text: &str| {
        o.push_str(&format!("                        Text {{\n"));
        o.push_str(&format!("                            x: 12; width: sidebar.width - 12; height: 28\n"));
        o.push_str(&format!("                            text: \"{text}\"\n"));
        o.push_str("                            color: root.colMuted\n");
        o.push_str("                            font.pixelSize: 10\n");
        o.push_str("                            font.letterSpacing: 1\n");
        o.push_str("                            font.bold: true\n");
        o.push_str("                            verticalAlignment: Text.AlignVCenter\n");
        o.push_str("                        }\n");
    };
    let emit_divider = |o: &mut String| {
        o.push_str("                        Rectangle {\n");
        o.push_str("                            width: sidebar.width\n");
        o.push_str("                            height: 1\n");
        o.push_str("                            color: root.colBorder\n");
        o.push_str("                        }\n");
    };

    let n_fetches = fetches.len();
    let n_instructions = idl.instructions.len();
    let wallet_idx = n_fetches + n_instructions;
    let settings_idx = n_fetches + n_instructions + 1;

    // Accounts section
    if n_fetches > 0 {
        emit_section_label(&mut o, "ACCOUNTS");
        for (i, f) in fetches.iter().enumerate() {
            emit_nav_item(&mut o, &title_case(&f.acc_name), i);
        }
        emit_divider(&mut o);
    }

    // Instructions section
    emit_section_label(&mut o, "INSTRUCTIONS");
    for (i, ix) in idl.instructions.iter().enumerate() {
        emit_nav_item(&mut o, &title_case(&ix.name), n_fetches + i);
    }
    // Wallet
    emit_section_label(&mut o, "WALLET");
    emit_nav_item(&mut o, "Wallet", wallet_idx);
    emit_divider(&mut o);

    // Settings
    emit_section_label(&mut o, "SETTINGS");
    emit_nav_item(&mut o, "Settings", settings_idx);

    o.push_str("                    }\n");   // Column
    o.push_str("                }\n");       // ScrollView
    o.push_str("            }\n");           // ColumnLayout
    o.push_str("        }\n\n");            // Rectangle#sidebar

    // ── Content pages ──────────────────────────────────────────────────────────
    o.push_str("        // ── Content pages ────────────────────────────────────────────────\n");
    o.push_str("        StackLayout {\n");
    o.push_str("            id: pageStack\n");
    o.push_str("            anchors { left: sidebar.right; right: parent.right; top: parent.top; bottom: parent.bottom }\n");
    o.push_str("            currentIndex: root.currentPageIndex\n\n");

    for f in fetches {
        qml_fetch_page(&mut o, f);
    }
    for ix in &idl.instructions {
        qml_instruction_page(&mut o, ix, idl);
    }
    qml_wallet_page(&mut o);
    qml_settings_page(&mut o);

    o.push_str("        }\n\n");    // StackLayout

    qml_toast(&mut o);

    o.push_str("    }\n"); // Rectangle

    // Zero-size TextEdit clipboard helper — must NOT use visible:false because
    // Qt won't let invisible elements interact with the clipboard.
    o.push_str("    TextEdit {\n");
    o.push_str("        id: clipHelper; width: 0; height: 0; opacity: 0\n");
    o.push_str("        function copyText(t) { clipHelper.text = t; selectAll(); copy() }\n");
    o.push_str("    }\n\n");

    o.push_str("}\n"); // Item

    o
}

/// Persistent label above a field so the name stays visible while typing.
fn qml_field_label(o: &mut String, label: &str, ind: &str) {
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"{label}\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted\n"));
    o.push_str(&format!("{ind}                font.pixelSize: 11\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n"));
}

/// Checkbox + label row for Option fields ("field_id_enabled" drives the field below).
fn qml_option_label_row(o: &mut String, field_id: &str, label: &str, ind: &str) {
    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                CheckBox {{ id: {field_id}_enabled; checked: false }}\n"));
    o.push_str(&format!("{ind}                Text {{\n"));
    o.push_str(&format!("{ind}                    text: \"{label} (optional)\"\n"));
    o.push_str(&format!("{ind}                    color: root.colMuted\n"));
    o.push_str(&format!("{ind}                    font.pixelSize: 11\n"));
    o.push_str(&format!("{ind}                    verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n"));
}

/// ComboBox populated from known enum variants; optional enable-gating.
fn qml_combobox(
    o: &mut String, id: &str, variants: &[&str], is_opt: bool, ind: &str,
) {
    let model = variants.iter().map(|v| format!("\"{v}\"")).collect::<Vec<_>>().join(", ");
    o.push_str(&format!("{ind}            ComboBox {{\n"));
    o.push_str(&format!("{ind}                id: {id}\n"));
    o.push_str(&format!("{ind}                model: [{model}]\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                Layout.rightMargin: 24\n"));
    if is_opt {
        o.push_str(&format!("{ind}                enabled: {id}_enabled.checked\n"));
        o.push_str(&format!("{ind}                opacity: enabled ? 1.0 : 0.4\n"));
    }
    o.push_str(&format!("{ind}            }}\n\n"));
}

fn qml_textfield_page(o: &mut String, id: &str, placeholder: &str, ind: &str) {
    o.push_str(&format!("{ind}            TextField {{\n"));
    o.push_str(&format!("{ind}                id: {id}\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                Layout.rightMargin: 24\n"));
    if !placeholder.is_empty() {
        o.push_str(&format!("{ind}                placeholderText: \"{placeholder}\"\n"));
    }
    o.push_str(&format!("{ind}                color: root.colText\n"));
    o.push_str(&format!("{ind}                placeholderTextColor: root.colMuted\n"));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                    color: root.colSurface\n"));
    o.push_str(&format!("{ind}                    border.color: root.colBorder\n"));
    o.push_str(&format!("{ind}                    radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
}

/// Account field: TextField + "▾" button that opens a two-section popup
/// (WALLET accounts from backend + RECENT history per field key).
fn qml_account_picker(o: &mut String, id: &str, hist_key: &str, ind: &str) {
    let popup = format!("{id}Popup");
    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                spacing: 4\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}                TextField {{\n"));
    o.push_str(&format!("{ind}                    id: {id}\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    placeholderText: \"base58 or 0x… hex\"\n"));
    o.push_str(&format!("{ind}                    color: root.colText\n"));
    o.push_str(&format!("{ind}                    placeholderTextColor: root.colMuted\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: root.colSurface\n"));
    o.push_str(&format!("{ind}                        border.color: root.colBorder\n"));
    o.push_str(&format!("{ind}                        radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    onEditingFinished: if (text.trim() !== \"\") backend.saveHistory(\"{hist_key}\", text.trim())\n"));
    o.push_str(&format!("{ind}                }}\n"));
    // Drop-down button
    o.push_str(&format!("{ind}                Button {{\n"));
    o.push_str(&format!("{ind}                    id: {id}Btn\n"));
    o.push_str(&format!("{ind}                    implicitWidth: 28\n"));
    o.push_str(&format!("{ind}                    implicitHeight: {id}.implicitHeight\n"));
    o.push_str(&format!("{ind}                    text: \"\\u25be\"\n")); // ▾
    o.push_str(&format!("{ind}                    background: Rectangle {{ color: root.colSurface; border.color: root.colBorder; radius: root.radius / 2 }}\n"));
    o.push_str(&format!("{ind}                    contentItem: Text {{ text: parent.text; color: root.colMuted; horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter }}\n"));
    o.push_str(&format!("{ind}                    onClicked: {popup}.open()\n"));
    o.push_str(&format!("{ind}                }}\n"));
    // Popup — padding:0 so Column.width === popup.width
    o.push_str(&format!("{ind}                Popup {{\n"));
    o.push_str(&format!("{ind}                    id: {popup}\n"));
    o.push_str(&format!("{ind}                    y: parent.height; x: 0\n"));
    o.push_str(&format!("{ind}                    width: parent.width\n"));
    o.push_str(&format!("{ind}                    padding: 0\n"));
    o.push_str(&format!("{ind}                    property var recentHistory: []\n"));
    o.push_str(&format!("{ind}                    onAboutToShow: recentHistory = backend.fieldHistory(\"{hist_key}\")\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{ color: root.colSidebar; border.color: root.colBorder; radius: root.radius / 2 }}\n"));
    // Column is a direct child — avoids contentItem width resolution issues
    o.push_str(&format!("{ind}                    Column {{\n"));
    o.push_str(&format!("{ind}                        width: parent.width\n"));
    o.push_str(&format!("{ind}                        spacing: 0\n"));
    o.push_str(&format!("{ind}                        topPadding: 4; bottomPadding: 4\n"));
    // RECENT section (first — most immediately useful)
    o.push_str(&format!("{ind}                        Text {{\n"));
    o.push_str(&format!("{ind}                            text: \"RECENT\"\n"));
    o.push_str(&format!("{ind}                            visible: {popup}.recentHistory.length > 0\n"));
    o.push_str(&format!("{ind}                            color: root.colMuted; font.pixelSize: 10; font.bold: true\n"));
    o.push_str(&format!("{ind}                            leftPadding: 8; topPadding: 2; bottomPadding: 2; width: parent.width\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                        Repeater {{\n"));
    o.push_str(&format!("{ind}                            model: {popup}.recentHistory\n"));
    o.push_str(&format!("{ind}                            delegate: Rectangle {{\n"));
    o.push_str(&format!("{ind}                                width: parent.width; height: 34\n"));
    o.push_str(&format!("{ind}                                color: _ma.containsMouse ? root.colSurface : \"transparent\"\n"));
    o.push_str(&format!("{ind}                                Text {{\n"));
    o.push_str(&format!("{ind}                                    anchors.verticalCenter: parent.verticalCenter\n"));
    o.push_str(&format!("{ind}                                    x: 8; width: parent.width - 8\n"));
    o.push_str(&format!("{ind}                                    text: modelData; color: root.colText; elide: Text.ElideMiddle; font.pixelSize: 13\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                                MouseArea {{\n"));
    o.push_str(&format!("{ind}                                    id: _ma; anchors.fill: parent\n"));
    o.push_str(&format!("{ind}                                    hoverEnabled: true; cursorShape: Qt.PointingHandCursor\n"));
    o.push_str(&format!("{ind}                                    onClicked: {{ {id}.text = modelData; {popup}.close() }}\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // Separator
    o.push_str(&format!("{ind}                        Rectangle {{\n"));
    o.push_str(&format!("{ind}                            width: parent.width; height: 1; color: root.colBorder\n"));
    o.push_str(&format!("{ind}                            visible: {popup}.recentHistory.length > 0 && backend.walletAccounts.length > 0\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // WALLET section
    o.push_str(&format!("{ind}                        Text {{\n"));
    o.push_str(&format!("{ind}                            text: \"WALLET\"\n"));
    o.push_str(&format!("{ind}                            visible: backend.walletAccounts.length > 0\n"));
    o.push_str(&format!("{ind}                            color: root.colMuted; font.pixelSize: 10; font.bold: true\n"));
    o.push_str(&format!("{ind}                            leftPadding: 8; topPadding: 2; bottomPadding: 2; width: parent.width\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // WALLET items — plain Rectangle+MouseArea avoids system styling
    o.push_str(&format!("{ind}                        Repeater {{\n"));
    o.push_str(&format!("{ind}                            model: backend.walletAccounts\n"));
    o.push_str(&format!("{ind}                            delegate: Rectangle {{\n"));
    o.push_str(&format!("{ind}                                width: parent.width; height: 34\n"));
    o.push_str(&format!("{ind}                                color: _ma.containsMouse ? root.colSurface : \"transparent\"\n"));
    o.push_str(&format!("{ind}                                Text {{\n"));
    o.push_str(&format!("{ind}                                    anchors.verticalCenter: parent.verticalCenter\n"));
    o.push_str(&format!("{ind}                                    x: 8; width: parent.width - 8\n"));
    o.push_str(&format!("{ind}                                    text: modelData.id + (modelData.label ? \" <b>[\" + modelData.label + \"]</b>\" : \"\")\n"));
    o.push_str(&format!("{ind}                                    textFormat: Text.StyledText\n"));
    o.push_str(&format!("{ind}                                    color: root.colText; elide: Text.ElideMiddle; font.pixelSize: 13\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                                MouseArea {{\n"));
    o.push_str(&format!("{ind}                                    id: _ma; anchors.fill: parent\n"));
    o.push_str(&format!("{ind}                                    hoverEnabled: true; cursorShape: Qt.PointingHandCursor\n"));
    o.push_str(&format!("{ind}                                    onClicked: {{ {id}.text = modelData.id; backend.saveHistory(\"{hist_key}\", modelData.id); {popup}.close() }}\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // Empty state
    o.push_str(&format!("{ind}                        Item {{\n"));
    o.push_str(&format!("{ind}                            visible: backend.walletAccounts.length === 0 && {popup}.recentHistory.length === 0\n"));
    o.push_str(&format!("{ind}                            height: 36; width: parent.width\n"));
    o.push_str(&format!("{ind}                            Text {{ anchors.centerIn: parent; text: \"No accounts or history yet.\"; color: root.colMuted; font.pixelSize: 11 }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                    }}\n")); // Column
    o.push_str(&format!("{ind}                }}\n")); // Popup
    o.push_str(&format!("{ind}            }}\n\n")); // RowLayout
}

/// Text field with a "▾" button that opens a RECENT-only history popup.
/// Used for non-account arg fields (amounts, strings, etc.).
fn qml_textfield_with_history(
    o: &mut String, id: &str, ty: &IdlType, hist_key: &str, is_opt: bool, ind: &str,
) {
    let placeholder = type_placeholder(ty);
    let val = validator_str(ty);
    let hints = input_hints_str(ty);
    let popup = format!("{id}Popup");
    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                spacing: 4\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                Layout.rightMargin: 24\n"));
    if is_opt {
        o.push_str(&format!("{ind}                enabled: {id}_enabled.checked\n"));
        o.push_str(&format!("{ind}                opacity: enabled ? 1.0 : 0.4\n"));
    }
    o.push_str(&format!("{ind}                TextField {{\n"));
    o.push_str(&format!("{ind}                    id: {id}\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    if !placeholder.is_empty() {
        o.push_str(&format!("{ind}                    placeholderText: \"{placeholder}\"\n"));
    }
    if let Some(v) = val {
        o.push_str(&format!("{ind}                    validator: {v}\n"));
    }
    if let Some(h) = hints {
        o.push_str(&format!("{ind}                    inputMethodHints: {h}\n"));
    }
    o.push_str(&format!("{ind}                    color: root.colText\n"));
    o.push_str(&format!("{ind}                    placeholderTextColor: root.colMuted\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: root.colSurface\n"));
    o.push_str(&format!("{ind}                        border.color: root.colBorder\n"));
    o.push_str(&format!("{ind}                        radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    onEditingFinished: if (text.trim() !== \"\") backend.saveHistory(\"{hist_key}\", text.trim())\n"));
    o.push_str(&format!("{ind}                }}\n"));
    // Drop-down button
    o.push_str(&format!("{ind}                Button {{\n"));
    o.push_str(&format!("{ind}                    implicitWidth: 28\n"));
    o.push_str(&format!("{ind}                    implicitHeight: {id}.implicitHeight\n"));
    o.push_str(&format!("{ind}                    text: \"\\u25be\"\n")); // ▾
    o.push_str(&format!("{ind}                    background: Rectangle {{ color: root.colSurface; border.color: root.colBorder; radius: root.radius / 2 }}\n"));
    o.push_str(&format!("{ind}                    contentItem: Text {{ text: parent.text; color: root.colMuted; horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter }}\n"));
    o.push_str(&format!("{ind}                    onClicked: {popup}.open()\n"));
    o.push_str(&format!("{ind}                }}\n"));
    // Popup — padding:0 so Column.width === popup.width
    o.push_str(&format!("{ind}                Popup {{\n"));
    o.push_str(&format!("{ind}                    id: {popup}\n"));
    o.push_str(&format!("{ind}                    y: parent.height; x: 0\n"));
    o.push_str(&format!("{ind}                    width: parent.width\n"));
    o.push_str(&format!("{ind}                    padding: 0\n"));
    o.push_str(&format!("{ind}                    property var recentHistory: []\n"));
    o.push_str(&format!("{ind}                    onAboutToShow: recentHistory = backend.fieldHistory(\"{hist_key}\")\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{ color: root.colSidebar; border.color: root.colBorder; radius: root.radius / 2 }}\n"));
    o.push_str(&format!("{ind}                    Column {{\n"));
    o.push_str(&format!("{ind}                        width: parent.width\n"));
    o.push_str(&format!("{ind}                        spacing: 0\n"));
    o.push_str(&format!("{ind}                        topPadding: 4; bottomPadding: 4\n"));
    o.push_str(&format!("{ind}                        Text {{\n"));
    o.push_str(&format!("{ind}                            text: \"RECENT\"\n"));
    o.push_str(&format!("{ind}                            visible: {popup}.recentHistory.length > 0\n"));
    o.push_str(&format!("{ind}                            color: root.colMuted; font.pixelSize: 10; font.bold: true\n"));
    o.push_str(&format!("{ind}                            leftPadding: 8; topPadding: 2; bottomPadding: 2; width: parent.width\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                        Repeater {{\n"));
    o.push_str(&format!("{ind}                            model: {popup}.recentHistory\n"));
    o.push_str(&format!("{ind}                            delegate: Rectangle {{\n"));
    o.push_str(&format!("{ind}                                width: parent.width; height: 34\n"));
    o.push_str(&format!("{ind}                                color: _ma.containsMouse ? root.colSurface : \"transparent\"\n"));
    o.push_str(&format!("{ind}                                Text {{\n"));
    o.push_str(&format!("{ind}                                    anchors.verticalCenter: parent.verticalCenter\n"));
    o.push_str(&format!("{ind}                                    x: 8; width: parent.width - 8\n"));
    o.push_str(&format!("{ind}                                    text: modelData; color: root.colText; elide: Text.ElideMiddle; font.pixelSize: 13\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                                MouseArea {{\n"));
    o.push_str(&format!("{ind}                                    id: _ma; anchors.fill: parent\n"));
    o.push_str(&format!("{ind}                                    hoverEnabled: true; cursorShape: Qt.PointingHandCursor\n"));
    o.push_str(&format!("{ind}                                    onClicked: {{ {id}.text = modelData; {popup}.close() }}\n"));
    o.push_str(&format!("{ind}                                }}\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                        Item {{\n"));
    o.push_str(&format!("{ind}                            visible: {popup}.recentHistory.length === 0\n"));
    o.push_str(&format!("{ind}                            height: 36; width: parent.width\n"));
    o.push_str(&format!("{ind}                            Text {{ anchors.centerIn: parent; text: \"No recent values.\"; color: root.colMuted; font.pixelSize: 11 }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                    }}\n")); // Column
    o.push_str(&format!("{ind}                }}\n")); // Popup
    o.push_str(&format!("{ind}            }}\n\n")); // RowLayout
}

fn qml_textarea_page(o: &mut String, id: &str, placeholder: &str, ind: &str) {
    o.push_str(&format!("{ind}            TextArea {{\n"));
    o.push_str(&format!("{ind}                id: {id}\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}                implicitHeight: 72\n"));
    o.push_str(&format!("{ind}                placeholderText: \"{placeholder} (one per line)\"\n"));
    o.push_str(&format!("{ind}                color: root.colText; wrapMode: TextArea.Wrap\n"));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                    color: root.colSurface\n"));
    o.push_str(&format!("{ind}                    border.color: root.colBorder\n"));
    o.push_str(&format!("{ind}                    radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
}

fn qml_instruction_page(o: &mut String, ix: &IdlInstruction, idl: &SpelIdl) {
    let params = instruction_params(ix);
    let method = camel_case(&ix.name);
    let title = title_case(&ix.name);
    let page_id = format!("page{}", pascal_case(&ix.name));
    let ind = "            ";

    o.push_str(&format!("{ind}Item {{\n"));
    o.push_str(&format!("{ind}    id: {page_id}\n"));
    o.push_str(&format!("{ind}    ScrollView {{\n"));
    o.push_str(&format!("{ind}        anchors.fill: parent; clip: true\n"));
    o.push_str(&format!("{ind}        contentWidth: availableWidth\n\n"));
    o.push_str(&format!("{ind}        ColumnLayout {{\n"));
    o.push_str(&format!("{ind}            width: {page_id}.width; spacing: 12\n\n"));
    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 24 }}\n\n"));

    // Title
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"{title}\"\n"));
    o.push_str(&format!("{ind}                color: root.colText\n"));
    o.push_str(&format!("{ind}                font.pixelSize: 18\n"));
    o.push_str(&format!("{ind}                font.bold: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Fields
    for p in &params {
        let field_id = format!("{}_{}f", snake_case(&ix.name), snake_case(&p.qt_name));

        // Peel Option wrapper to inspect the core type.
        let (is_opt, core_ty): (bool, Option<&IdlType>) = match &p.kind {
            ParamKind::Arg(IdlType::Option { option }) => (true, Some(option.as_ref())),
            ParamKind::Arg(ty) => (false, Some(ty)),
            ParamKind::Account => (false, None),
        };
        let variants = core_ty.and_then(|ty| enum_variants(ty, idl));

        match (&p.kind, &variants) {
            // ── bool: inline CheckBox row (label acts as the checkbox text) ──────
            (ParamKind::Arg(ty), None) if is_bool_type(ty) => {
                o.push_str(&format!("{ind}            RowLayout {{\n"));
                o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
                o.push_str(&format!("{ind}                Layout.rightMargin: 24\n"));
                o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
                o.push_str(&format!("{ind}                CheckBox {{ id: {field_id}; checked: false }}\n"));
                o.push_str(&format!("{ind}                Text {{ text: \"{}\"; color: root.colText; font.pixelSize: 13 }}\n", p.qt_name));
                o.push_str(&format!("{ind}            }}\n\n"));
            }
            // ── Vec<T>: label + multiline TextArea ──────────────────────────────
            (ParamKind::Arg(ty), None) if is_list_type(ty) => {
                qml_field_label(o, &p.qt_name, ind);
                qml_textarea_page(o, &field_id, &p.qt_name, ind);
            }
            // ── Enum Defined (non-optional): label + ComboBox ───────────────────
            (_, Some(vs)) if !is_opt => {
                qml_field_label(o, &p.qt_name, ind);
                qml_combobox(o, &field_id, vs, false, ind);
            }
            // ── Option<Enum>: checkbox label + disabled ComboBox ────────────────
            (_, Some(vs)) => {
                qml_option_label_row(o, &field_id, &p.qt_name, ind);
                qml_combobox(o, &field_id, vs, true, ind);
            }
            // ── Account signer: label + picker (WALLET accounts + RECENT history) ─
            (ParamKind::Account, None) => {
                qml_field_label(o, &p.qt_name, ind);
                qml_account_picker(o, &field_id, &field_id, ind);
            }
            // ── Option<T>: checkbox label + disabled field with history ───────────
            (ParamKind::Arg(_), None) if is_opt => {
                qml_option_label_row(o, &field_id, &p.qt_name, ind);
                qml_textfield_with_history(o, &field_id, core_ty.unwrap(), &field_id, true, ind);
            }
            // ── Regular T: label + field with history ────────────────────────────
            (ParamKind::Arg(ty), None) => {
                qml_field_label(o, &p.qt_name, ind);
                qml_textfield_with_history(o, &field_id, ty, &field_id, false, ind);
            }
        }
    }

    // Submit button
    let call_args = params
        .iter()
        .map(|p| {
            let fid = format!("{}_{}f", snake_case(&ix.name), snake_case(&p.qt_name));
            qml_field_expr(&p.kind, &fid, idl)
        })
        .collect::<Vec<_>>()
        .join(", ");

    o.push_str(&format!("{ind}            Button {{\n"));
    o.push_str(&format!("{ind}                text: backend.busy ? \"\\u2026\" : \"{title}\"\n"));
    o.push_str(&format!("{ind}                enabled: !backend.busy\n"));
    o.push_str(&format!("{ind}                Layout.rightMargin: 24; Layout.alignment: Qt.AlignRight\n"));
    o.push_str(&format!("{ind}                onClicked: backend.{method}({call_args})\n"));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                    color: parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary\n"));
    o.push_str(&format!("{ind}                    radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    opacity: parent.enabled ? 1.0 : 0.5\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                    text: parent.text; color: root.colText\n"));
    o.push_str(&format!("{ind}                    horizontalAlignment: Text.AlignHCenter\n"));
    o.push_str(&format!("{ind}                    verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 80 }}\n"));
    o.push_str(&format!("{ind}        }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}    }}\n"));     // ScrollView
    o.push_str(&format!("{ind}}}\n\n"));       // Item
}

fn qml_fetch_page(o: &mut String, f: &FetchAccount) {
    let prop = camel_case(&f.acc_name);
    let title = title_case(&f.acc_name);
    let page_id = format!("pageFetch{}", pascal_case(&f.acc_name));
    let fetch_method = format!("fetch{}", pascal_case(&f.acc_name));
    let ind = "            ";

    // Fetch pages use colSurface so they're visually distinct from instruction pages.
    o.push_str(&format!("{ind}Item {{\n"));
    o.push_str(&format!("{ind}    id: {page_id}\n"));
    o.push_str(&format!("{ind}    Rectangle {{ anchors.fill: parent; color: root.colSurface }}\n"));

    let seed_call = f
        .seed_params
        .iter()
        .map(|(name, ty)| {
            let fid = format!("fetch{}_{}_f", pascal_case(&f.acc_name), snake_case(name));
            match ty {
                IdlType::Primitive(p) => match p.as_str() {
                    "bool" => format!("{fid}.checked"),
                    "u8" | "u16" | "u32" | "i8" | "i16" | "i32" => {
                        format!("parseInt({fid}.text)")
                    }
                    // 64/128-bit integers: pass as string to avoid IEEE-754 precision loss
                    "u64" | "i64" | "u128" | "i128" => format!("{fid}.text"),
                    _ => format!("{fid}.text"),
                },
                IdlType::Vec { .. } => format!(
                    "{fid}.text.split(\"\\n\").map(function(s){{ return s.trim() }}).filter(function(s){{ return s.length > 0 }})"
                ),
                _ => format!("{fid}.text"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    o.push_str(&format!("{ind}    ScrollView {{\n"));
    o.push_str(&format!("{ind}        anchors.fill: parent; clip: true\n"));
    o.push_str(&format!("{ind}        contentWidth: availableWidth\n\n"));
    o.push_str(&format!("{ind}        ColumnLayout {{\n"));
    o.push_str(&format!("{ind}            width: {page_id}.width; spacing: 12\n\n"));
    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 24 }}\n\n"));

    // Title row with refresh button
    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}                Text {{\n"));
    o.push_str(&format!("{ind}                    text: \"{title}\"\n"));
    o.push_str(&format!("{ind}                    color: root.colText\n"));
    o.push_str(&format!("{ind}                    font.pixelSize: 18; font.bold: true\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                Button {{\n"));
    o.push_str(&format!("{ind}                    text: \"\\u21ba\"\n"));
    o.push_str(&format!("{ind}                    onClicked: backend.{fetch_method}({seed_call})\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: root.colSurface; border.color: root.colBorder; radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                        text: parent.text; color: root.colMuted\n"));
    o.push_str(&format!("{ind}                        horizontalAlignment: Text.AlignHCenter\n"));
    o.push_str(&format!("{ind}                        verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                }}\n")); // Button
    o.push_str(&format!("{ind}            }}\n\n")); // RowLayout

    // Seed input fields
    for (name, ty) in &f.seed_params {
        let fid = format!("fetch{}_{}_f", pascal_case(&f.acc_name), snake_case(name));
        let label = format!("{} (seed)", camel_case(name));
        if is_list_type(ty) {
            qml_textarea_page(o, &fid, &label, ind);
        } else {
            qml_textfield_page(o, &fid, &label, ind);
        }
    }

    // Key-value display with per-row copy button
    o.push_str(&format!("{ind}            Repeater {{\n"));
    o.push_str(&format!("{ind}                model: Object.keys(backend.{prop})\n"));
    o.push_str(&format!("{ind}                delegate: RowLayout {{\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                    Layout.rightMargin: 8\n"));
    o.push_str(&format!("{ind}                    Text {{ text: modelData + \":\"; color: root.colMuted; font.pixelSize: 12; Layout.preferredWidth: 140 }}\n"));
    o.push_str(&format!("{ind}                    Text {{\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.{prop}[modelData]\n"));
    o.push_str(&format!("{ind}                        text: Array.isArray(_v) ? _v.join(\"\\n\") : (_v ?? \"\")\n"));
    o.push_str(&format!("{ind}                        color: root.colText; font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere\n"));
    o.push_str(&format!("{ind}                        Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    Button {{\n"));
    o.push_str(&format!("{ind}                        implicitWidth: 28; implicitHeight: 28\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.{prop}[modelData]\n"));
    o.push_str(&format!("{ind}                        property bool _copied: false\n"));
    o.push_str(&format!("{ind}                        onClicked: {{ clipHelper.copyText(Array.isArray(_v) ? _v.join(\"\\n\") : (_v ?? \"\")); _copied = true; _copyReset.restart() }}\n"));
    o.push_str(&format!("{ind}                        Timer {{ id: _copyReset; interval: 1500; onTriggered: parent._copied = false }}\n"));
    o.push_str(&format!("{ind}                        background: Item {{}}\n"));
    o.push_str(&format!("{ind}                        contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                            text: parent._copied ? \"\\u2713\" : \"\\u29C9\"\n"));
    o.push_str(&format!("{ind}                            color: parent._copied ? root.colSuccess : root.colMuted\n"));
    o.push_str(&format!("{ind}                            font.pixelSize: 14\n"));
    o.push_str(&format!("{ind}                            horizontalAlignment: Text.AlignHCenter\n"));
    o.push_str(&format!("{ind}                            verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                    }}\n")); // Button
    o.push_str(&format!("{ind}                }}\n")); // delegate
    o.push_str(&format!("{ind}            }}\n\n")); // Repeater

    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: Object.keys(backend.{prop}).length === 0\n"));
    o.push_str(&format!("{ind}                text: \"No data — press \\u21ba to fetch.\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 80 }}\n"));
    o.push_str(&format!("{ind}        }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}    }}\n"));     // ScrollView
    o.push_str(&format!("{ind}}}\n\n"));       // Item
}

fn qml_wallet_page(o: &mut String) {
    let ind = "            ";
    o.push_str(&format!("{ind}Item {{\n"));
    o.push_str(&format!("{ind}    id: pageWallet\n"));
    o.push_str(&format!("{ind}    Rectangle {{ anchors.fill: parent; color: root.colSurface }}\n"));
    o.push_str(&format!("{ind}    ScrollView {{\n"));
    o.push_str(&format!("{ind}        anchors.fill: parent; clip: true\n"));
    o.push_str(&format!("{ind}        contentWidth: availableWidth\n\n"));
    o.push_str(&format!("{ind}        ColumnLayout {{\n"));
    o.push_str(&format!("{ind}            width: pageWallet.width; spacing: 12\n\n"));
    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 24 }}\n\n"));

    // Title
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"Wallet\"\n"));
    o.push_str(&format!("{ind}                color: root.colText; font.pixelSize: 18; font.bold: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // ── Section: Connection ──────────────────────────────────────────────
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"CONNECTION\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 10; font.bold: true; font.letterSpacing: 1\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.topMargin: 8\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Button {{\n"));
    o.push_str(&format!("{ind}                text: \"Check Connection\"\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                onClicked: backend.checkConnection()\n"));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                    color: parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary\n"));
    o.push_str(&format!("{ind}                    radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                    text: parent.text; color: root.colText\n"));
    o.push_str(&format!("{ind}                    horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: backend.connectionStatus || \"Not checked\"\n"));
    o.push_str(&format!("{ind}                color: backend.connectionStatus.startsWith(\"\\u2713\") ? root.colSuccess\n"));
    o.push_str(&format!("{ind}                     : backend.connectionStatus.startsWith(\"\\u2717\") ? root.colError\n"));
    o.push_str(&format!("{ind}                     : root.colMuted\n"));
    o.push_str(&format!("{ind}                font.pixelSize: 13; Layout.leftMargin: 24; Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                wrapMode: Text.WrapAtWordBoundaryOrAnywhere\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Divider
    o.push_str(&format!("{ind}            Rectangle {{ Layout.fillWidth: true; height: 1; color: root.colBorder; Layout.topMargin: 8 }}\n\n"));

    // ── Section: Accounts ────────────────────────────────────────────────
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"ACCOUNTS\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 10; font.bold: true; font.letterSpacing: 1\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.topMargin: 8\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Refresh button
    o.push_str(&format!("{ind}            Button {{\n"));
    o.push_str(&format!("{ind}                text: \"\\u21ba Refresh\"\n")); // ↺ Refresh
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}                onClicked: backend.listAccounts()\n"));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                    color: parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary; radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                    text: parent.text; color: root.colText\n"));
    o.push_str(&format!("{ind}                    horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Account list
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: backend.walletAccounts.length === 0\n"));
    o.push_str(&format!("{ind}                text: \"No accounts — press Refresh.\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 12; Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Repeater {{\n"));
    o.push_str(&format!("{ind}                model: backend.walletAccounts\n"));
    o.push_str(&format!("{ind}                delegate: ColumnLayout {{\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true; spacing: 2\n"));
    o.push_str(&format!("{ind}                    RowLayout {{\n"));
    o.push_str(&format!("{ind}                        Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 8\n"));
    // Status badge — color-coded by owner classification
    o.push_str(&format!("{ind}                        Rectangle {{\n"));
    o.push_str(&format!("{ind}                            property string _st: modelData[\"status\"] || \"unknown\"\n"));
    o.push_str(&format!("{ind}                            width: statusLabel.implicitWidth + 10; height: 18; radius: 9\n"));
    o.push_str(&format!("{ind}                            color: _st === \"uninitialized\" ? Qt.rgba(0.6, 0.6, 0.6, 0.2)\n"));
    o.push_str(&format!("{ind}                                 : _st === \"owned\"         ? Qt.rgba(0.2, 0.8, 0.4, 0.2)\n"));
    o.push_str(&format!("{ind}                                 : _st === \"foreign\"       ? Qt.rgba(0.3, 0.5, 1.0, 0.2)\n"));
    o.push_str(&format!("{ind}                                 : Qt.rgba(1.0, 0.6, 0.0, 0.2)\n"));
    o.push_str(&format!("{ind}                            Text {{\n"));
    o.push_str(&format!("{ind}                                id: statusLabel\n"));
    o.push_str(&format!("{ind}                                anchors.centerIn: parent\n"));
    o.push_str(&format!("{ind}                                text: parent._st === \"uninitialized\" ? \"free\"\n"));
    o.push_str(&format!("{ind}                                     : parent._st === \"owned\"         ? \"owned\"\n"));
    o.push_str(&format!("{ind}                                     : parent._st === \"foreign\"       ? \"other\"\n"));
    o.push_str(&format!("{ind}                                     : \"?\"\n"));
    o.push_str(&format!("{ind}                                color: parent._st === \"uninitialized\" ? root.colMuted\n"));
    o.push_str(&format!("{ind}                                     : parent._st === \"owned\"         ? root.colSuccess\n"));
    o.push_str(&format!("{ind}                                     : parent._st === \"foreign\"       ? root.colPrimary\n"));
    o.push_str(&format!("{ind}                                     : \"#f0a030\"\n"));
    o.push_str(&format!("{ind}                                font.pixelSize: 10; font.bold: true\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n")); // Rectangle badge
    // Label/path
    o.push_str(&format!("{ind}                        Text {{\n"));
    o.push_str(&format!("{ind}                            property string _disp: modelData[\"label\"] || modelData[\"path\"] || \"\"\n"));
    o.push_str(&format!("{ind}                            text: _disp\n"));
    o.push_str(&format!("{ind}                            visible: _disp !== \"\"\n"));
    o.push_str(&format!("{ind}                            color: modelData[\"label\"] ? root.colPrimary : root.colMuted\n"));
    o.push_str(&format!("{ind}                            font.pixelSize: 11; Layout.preferredWidth: 80\n"));
    o.push_str(&format!("{ind}                            elide: Text.ElideRight\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // Account ID — click to populate inspect field
    o.push_str(&format!("{ind}                        Text {{\n"));
    o.push_str(&format!("{ind}                            text: modelData[\"id\"] || \"\"\n"));
    o.push_str(&format!("{ind}                            color: root.colText; font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}                            elide: Text.ElideMiddle; Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                            MouseArea {{\n"));
    o.push_str(&format!("{ind}                                anchors.fill: parent; cursorShape: Qt.PointingHandCursor\n"));
    o.push_str(&format!("{ind}                                onClicked: walletInspectId.text = modelData[\"id\"] || \"\"\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    // Copy button
    o.push_str(&format!("{ind}                        Button {{\n"));
    o.push_str(&format!("{ind}                            implicitWidth: 28; implicitHeight: 28\n"));
    o.push_str(&format!("{ind}                            property bool _copied: false\n"));
    o.push_str(&format!("{ind}                            onClicked: {{ clipHelper.copyText(modelData[\"id\"] || \"\"); _copied = true; _copyReset.restart() }}\n"));
    o.push_str(&format!("{ind}                            Timer {{ id: _copyReset; interval: 1500; onTriggered: parent._copied = false }}\n"));
    o.push_str(&format!("{ind}                            background: Item {{}}\n"));
    o.push_str(&format!("{ind}                            contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                                text: parent._copied ? \"\\u2713\" : \"\\u29C9\"\n"));
    o.push_str(&format!("{ind}                                color: parent._copied ? root.colSuccess : root.colMuted\n"));
    o.push_str(&format!("{ind}                                font.pixelSize: 14\n"));
    o.push_str(&format!("{ind}                                horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n")); // copy Button
    // Decode button — only for program-owned accounts
    o.push_str(&format!("{ind}                        Button {{\n"));
    o.push_str(&format!("{ind}                            visible: (modelData[\"status\"] || \"\") === \"owned\"\n"));
    o.push_str(&format!("{ind}                            implicitHeight: 22; implicitWidth: 52\n"));
    o.push_str(&format!("{ind}                            onClicked: backend.decodeAccount(modelData[\"id\"] || \"\")\n"));
    o.push_str(&format!("{ind}                            background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                                color: parent.down ? Qt.darker(root.colSuccess, 1.3) : Qt.rgba(0.2, 0.8, 0.4, 0.25)\n"));
    o.push_str(&format!("{ind}                                radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                            contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                                text: \"Decode\"; color: root.colSuccess; font.pixelSize: 11\n"));
    o.push_str(&format!("{ind}                                horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                            }}\n"));
    o.push_str(&format!("{ind}                        }}\n")); // decode Button
    o.push_str(&format!("{ind}                    }}\n")); // RowLayout
    o.push_str(&format!("{ind}                }}\n")); // ColumnLayout delegate
    o.push_str(&format!("{ind}            }}\n\n")); // Repeater

    // Divider before create section
    o.push_str(&format!("{ind}            Rectangle {{ Layout.fillWidth: true; height: 1; color: root.colBorder; Layout.topMargin: 8 }}\n\n"));

    // New account subsection
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"NEW ACCOUNT\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 10; font.bold: true; font.letterSpacing: 1\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.topMargin: 4\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}                TextField {{\n"));
    o.push_str(&format!("{ind}                    id: walletNewLabel\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    placeholderText: \"Label (optional)\"\n"));
    o.push_str(&format!("{ind}                    color: root.colText; placeholderTextColor: root.colMuted\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: root.colBg; border.color: root.colBorder; radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                Button {{\n"));
    o.push_str(&format!("{ind}                    text: \"+ Create\"\n"));
    o.push_str(&format!("{ind}                    onClicked: backend.createAccount(walletNewLabel.text)\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary; radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                        text: parent.text; color: root.colText\n"));
    o.push_str(&format!("{ind}                        horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Divider
    o.push_str(&format!("{ind}            Rectangle {{ Layout.fillWidth: true; height: 1; color: root.colBorder; Layout.topMargin: 8 }}\n\n"));

    // ── Section: Inspect Account ─────────────────────────────────────────
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"INSPECT ACCOUNT\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 10; font.bold: true; font.letterSpacing: 1\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.topMargin: 8\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Account ID input + Inspect button
    o.push_str(&format!("{ind}            RowLayout {{\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}                TextField {{\n"));
    o.push_str(&format!("{ind}                    id: walletInspectId\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    placeholderText: \"Account ID (base58 or 0x\\u2026 hex)\"\n"));
    o.push_str(&format!("{ind}                    color: root.colText; placeholderTextColor: root.colMuted\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: root.colBg; border.color: root.colBorder; radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                Button {{\n"));
    o.push_str(&format!("{ind}                    text: \"Inspect\"\n"));
    o.push_str(&format!("{ind}                    enabled: walletInspectId.text.length > 0\n"));
    o.push_str(&format!("{ind}                    onClicked: backend.inspectAccount(walletInspectId.text)\n"));
    o.push_str(&format!("{ind}                    background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                        color: parent.enabled ? (parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary) : root.colBorder\n"));
    o.push_str(&format!("{ind}                        radius: root.radius / 2\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                        text: parent.text; color: root.colText\n"));
    o.push_str(&format!("{ind}                        horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    // Account info repeater
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: Object.keys(backend.walletAccountInfo).length === 0\n"));
    o.push_str(&format!("{ind}                text: \"Enter an account ID and press Inspect.\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 12; Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    o.push_str(&format!("{ind}            Repeater {{\n"));
    o.push_str(&format!("{ind}                model: Object.keys(backend.walletAccountInfo)\n"));
    o.push_str(&format!("{ind}                delegate: RowLayout {{\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 8\n"));
    o.push_str(&format!("{ind}                    Text {{ text: modelData + \":\"; color: root.colMuted; font.pixelSize: 12; Layout.preferredWidth: 140 }}\n"));
    o.push_str(&format!("{ind}                    Text {{\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.walletAccountInfo[modelData]\n"));
    o.push_str(&format!("{ind}                        text: _v ?? \"\"; color: root.colText; font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere; Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    Button {{\n"));
    o.push_str(&format!("{ind}                        implicitWidth: 28; implicitHeight: 28\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.walletAccountInfo[modelData]\n"));
    o.push_str(&format!("{ind}                        property bool _copied: false\n"));
    o.push_str(&format!("{ind}                        onClicked: {{ clipHelper.copyText(_v ?? \"\"); _copied = true; _copyReset.restart() }}\n"));
    o.push_str(&format!("{ind}                        Timer {{ id: _copyReset; interval: 1500; onTriggered: parent._copied = false }}\n"));
    o.push_str(&format!("{ind}                        background: Item {{}}\n"));
    o.push_str(&format!("{ind}                        contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                            text: parent._copied ? \"\\u2713\" : \"\\u29C9\"\n"));
    o.push_str(&format!("{ind}                            color: parent._copied ? root.colSuccess : root.colMuted\n"));
    o.push_str(&format!("{ind}                            font.pixelSize: 14\n"));
    o.push_str(&format!("{ind}                            horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                    }}\n")); // Button
    o.push_str(&format!("{ind}                }}\n")); // delegate
    o.push_str(&format!("{ind}            }}\n\n")); // Repeater

    // ── Section: Decoded Data ────────────────────────────────────────────
    o.push_str(&format!("{ind}            Rectangle {{ Layout.fillWidth: true; height: 1; color: root.colBorder; Layout.topMargin: 8 }}\n\n"));
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"DECODED DATA\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 10; font.bold: true; font.letterSpacing: 1\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.topMargin: 8\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // Type name header when decode succeeded
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: (backend.walletDecodedAccount[\"type\"] || \"\") !== \"\"\n"));
    o.push_str(&format!("{ind}                text: \"Type: \" + (backend.walletDecodedAccount[\"type\"] || \"\")\n"));
    o.push_str(&format!("{ind}                color: root.colSuccess; font.pixelSize: 13; font.bold: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // Uninitialized notice
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: (backend.walletDecodedAccount[\"status\"] || \"\") === \"uninitialized\"\n"));
    o.push_str(&format!("{ind}                text: \"Account is uninitialized (no data).\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 12; Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // No matching type
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                property bool _noType: Object.keys(backend.walletDecodedAccount).length > 0\n"));
    o.push_str(&format!("{ind}                    && (backend.walletDecodedAccount[\"type\"] === undefined || backend.walletDecodedAccount[\"type\"] === null || backend.walletDecodedAccount[\"type\"] === \"\")\n"));
    o.push_str(&format!("{ind}                    && (backend.walletDecodedAccount[\"status\"] || \"\") !== \"uninitialized\"\n"));
    o.push_str(&format!("{ind}                    && (backend.walletDecodedAccount[\"error\"] || \"\") === \"\"\n"));
    o.push_str(&format!("{ind}                visible: _noType\n"));
    o.push_str(&format!("{ind}                text: \"No matching IDL type. Raw hex: \" + (backend.walletDecodedAccount[\"raw_hex\"] || \"\")\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 11; font.family: \"monospace\"\n"));
    o.push_str(&format!("{ind}                wrapMode: Text.WrapAtWordBoundaryOrAnywhere; Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // Error
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: (backend.walletDecodedAccount[\"error\"] || \"\") !== \"\"\n"));
    o.push_str(&format!("{ind}                text: backend.walletDecodedAccount[\"error\"] || \"\"\n"));
    o.push_str(&format!("{ind}                color: root.colError; font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}                wrapMode: Text.WrapAtWordBoundaryOrAnywhere; Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // Empty state hint
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                visible: Object.keys(backend.walletDecodedAccount).length === 0\n"));
    o.push_str(&format!("{ind}                text: \"Click \\\"Decode\\\" on a program-owned account to view its data.\"\n"));
    o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 12; Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));
    // Decoded fields as key-value rows
    o.push_str(&format!("{ind}            Repeater {{\n"));
    o.push_str(&format!("{ind}                model: Object.keys((backend.walletDecodedAccount[\"fields\"] instanceof Object) ? backend.walletDecodedAccount[\"fields\"] : {{}})\n"));
    o.push_str(&format!("{ind}                delegate: RowLayout {{\n"));
    o.push_str(&format!("{ind}                    Layout.fillWidth: true; Layout.leftMargin: 24; Layout.rightMargin: 8\n"));
    o.push_str(&format!("{ind}                    Text {{ text: modelData + \":\"; color: root.colMuted; font.pixelSize: 12; Layout.preferredWidth: 140 }}\n"));
    o.push_str(&format!("{ind}                    Text {{\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.walletDecodedAccount[\"fields\"][modelData]\n"));
    o.push_str(&format!("{ind}                        text: _v !== undefined && _v !== null ? String(_v) : \"\"\n"));
    o.push_str(&format!("{ind}                        color: root.colText; font.pixelSize: 12; font.family: \"monospace\"\n"));
    o.push_str(&format!("{ind}                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere; Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                    }}\n"));
    o.push_str(&format!("{ind}                    Button {{\n"));
    o.push_str(&format!("{ind}                        implicitWidth: 28; implicitHeight: 28\n"));
    o.push_str(&format!("{ind}                        property var _v: backend.walletDecodedAccount[\"fields\"][modelData]\n"));
    o.push_str(&format!("{ind}                        property bool _copied: false\n"));
    o.push_str(&format!("{ind}                        onClicked: {{ clipHelper.copyText(_v !== undefined ? String(_v) : \"\"); _copied = true; _copyReset.restart() }}\n"));
    o.push_str(&format!("{ind}                        Timer {{ id: _copyReset; interval: 1500; onTriggered: parent._copied = false }}\n"));
    o.push_str(&format!("{ind}                        background: Item {{}}\n"));
    o.push_str(&format!("{ind}                        contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                            text: parent._copied ? \"\\u2713\" : \"\\u29C9\"\n"));
    o.push_str(&format!("{ind}                            color: parent._copied ? root.colSuccess : root.colMuted\n"));
    o.push_str(&format!("{ind}                            font.pixelSize: 14\n"));
    o.push_str(&format!("{ind}                            horizontalAlignment: Text.AlignHCenter; verticalAlignment: Text.AlignVCenter\n"));
    o.push_str(&format!("{ind}                        }}\n"));
    o.push_str(&format!("{ind}                    }}\n")); // copy button
    o.push_str(&format!("{ind}                }}\n")); // delegate
    o.push_str(&format!("{ind}            }}\n\n")); // Repeater

    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 80 }}\n"));
    o.push_str(&format!("{ind}        }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}    }}\n"));     // ScrollView
    o.push_str(&format!("{ind}}}\n\n"));       // Item
}

fn qml_settings_page(o: &mut String) {
    let ind = "            ";
    // Settings page uses colSurface — same visual group as fetch/account pages.
    o.push_str(&format!("{ind}Item {{\n"));
    o.push_str(&format!("{ind}    id: pageSettings\n"));
    o.push_str(&format!("{ind}    Rectangle {{ anchors.fill: parent; color: root.colSurface }}\n"));
    o.push_str(&format!("{ind}    ScrollView {{\n"));
    o.push_str(&format!("{ind}        anchors.fill: parent; clip: true\n"));
    o.push_str(&format!("{ind}        contentWidth: availableWidth\n\n"));
    o.push_str(&format!("{ind}        ColumnLayout {{\n"));
    o.push_str(&format!("{ind}            width: pageSettings.width; spacing: 16\n\n"));
    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 24 }}\n\n"));
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!("{ind}                text: \"Settings\"\n"));
    o.push_str(&format!("{ind}                color: root.colText; font.pixelSize: 18; font.bold: true\n"));
    o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
    o.push_str(&format!("{ind}            }}\n\n"));

    for (label, prop, setter) in [
        ("Wallet Path",      "walletPath",   "setWalletPath"),
        ("Sequencer URL",    "sequencerUrl", "setSequencerUrl"),
        ("Program ID (hex)", "programIdHex", "setProgramIdHex"),
    ] {
        o.push_str(&format!("{ind}            Text {{\n"));
        o.push_str(&format!("{ind}                text: \"{label}\"\n"));
        o.push_str(&format!("{ind}                color: root.colMuted; font.pixelSize: 11\n"));
        o.push_str(&format!("{ind}                Layout.leftMargin: 24\n"));
        o.push_str(&format!("{ind}            }}\n"));
        o.push_str(&format!("{ind}            TextField {{\n"));
        o.push_str(&format!("{ind}                text: backend.{prop}\n"));
        o.push_str(&format!("{ind}                onEditingFinished: backend.{setter}(text)\n"));
        o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
        o.push_str(&format!("{ind}                Layout.leftMargin: 24; Layout.rightMargin: 24\n"));
        o.push_str(&format!("{ind}                color: root.colText; placeholderTextColor: root.colMuted\n"));
        o.push_str(&format!("{ind}                background: Rectangle {{\n"));
        o.push_str(&format!("{ind}                    color: root.colSurface; border.color: root.colBorder; radius: root.radius / 2\n"));
        o.push_str(&format!("{ind}                }}\n"));
        o.push_str(&format!("{ind}            }}\n\n"));
    }

    o.push_str(&format!("{ind}            Item {{ Layout.fillWidth: true; height: 80 }}\n"));
    o.push_str(&format!("{ind}        }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}    }}\n"));     // ScrollView
    o.push_str(&format!("{ind}}}\n\n"));       // Item
}

fn qml_toast(o: &mut String) {
    o.push_str("        Rectangle {\n");
    o.push_str("            id: toast\n");
    o.push_str("            anchors { bottom: parent.bottom; horizontalCenter: parent.horizontalCenter; bottomMargin: 24 }\n");
    o.push_str("            width: Math.min(toastText.implicitWidth + 48, parent.width - 80); height: 44\n");
    o.push_str("            radius: root.radius\n");
    o.push_str("            color: root.colSurface\n");
    o.push_str("            opacity: 0; visible: opacity > 0\n\n");
    o.push_str("            function show(msg, col, duration) {\n");
    o.push_str("                toastText.text = msg\n");
    o.push_str("                toast.color = col\n");
    o.push_str("                toast.opacity = 1\n");
    o.push_str("                toastTimer.interval = duration || 4000\n");
    o.push_str("                toastTimer.restart()\n");
    o.push_str("            }\n\n");
    o.push_str("            Text {\n");
    o.push_str("                id: toastText\n");
    o.push_str("                anchors { fill: parent; margins: 12 }\n");
    o.push_str("                color: root.colText\n");
    o.push_str("                font.pixelSize: 13\n");
    o.push_str("                wrapMode: Text.WordWrap\n");
    o.push_str("                horizontalAlignment: Text.AlignHCenter\n");
    o.push_str("                verticalAlignment: Text.AlignVCenter\n");
    o.push_str("            }\n\n");
    o.push_str("            MouseArea {\n");
    o.push_str("                anchors.fill: parent\n");
    o.push_str("                onClicked: { toastTimer.stop(); toast.opacity = 0 }\n");
    o.push_str("                cursorShape: Qt.PointingHandCursor\n");
    o.push_str("            }\n\n");
    o.push_str("            Behavior on opacity { NumberAnimation { duration: 350 } }\n\n");
    o.push_str("            Timer {\n");
    o.push_str("                id: toastTimer\n");
    o.push_str("                onTriggered: toast.opacity = 0\n");
    o.push_str("            }\n");
    o.push_str("        }\n");
}

// ── module.yaml ───────────────────────────────────────────────────────────────

fn gen_module_yaml(idl: &SpelIdl, effective_prog: &str, class: &str) -> String {
    let desc = format!("Qt/QML Basecamp module for the {} program", idl.name);
    let ver = &idl.version;
    let ffi = format!("{}_ffi", effective_prog);
    format!(
        "# Auto-generated by spel-client-gen --target logos-module.\n\
         name: {effective_prog}\n\
         version: {ver}\n\
         type: ui\n\
         category: tools\n\
         description: \"{desc}\"\n\n\
         dependencies: []\n\n\
         nix_packages:\n\
         \x20 build: []\n\
         \x20 runtime: []\n\n\
         external_libraries:\n\
         \x20 - name: {ffi}\n\
         \x20   vendor_path: lib\n\n\
         cmake:\n\
         \x20 find_packages: []\n\
         \x20 extra_sources:\n\
         \x20   - src/{class}Backend.cpp\n\
         \x20   - src/{class}Plugin.cpp\n\
         \x20 proto_files: []\n"
    )
}

// ── manifest.json ─────────────────────────────────────────────────────────────

fn gen_cmake_lists(class: &str, effective_prog: &str, ffi_lib_path: Option<&str>) -> String {
    // The Qt resource name MUST be kept in sync with Q_INIT_RESOURCE() in XyzPlugin.cpp.
    // Convention: <effective_prog>_qml — the _qml suffix avoids collisions with other targets.
    let res_name = format!("{}_qml", effective_prog.replace('-', "_"));

    let ffi_imported = if let Some(path) = ffi_lib_path {
        format!(
            "\n\
             # ── FFI shared library (built by `make ffi`) ──────────────────────────────────\n\
             add_library({effective_prog}_ffi SHARED IMPORTED)\n\
             set_target_properties({effective_prog}_ffi PROPERTIES\n\
             \x20   IMPORTED_LOCATION \"${{CMAKE_CURRENT_SOURCE_DIR}}/{path}\"\n\
             )\n"
        )
    } else {
        format!(
            "\n\
             # ── FFI shared library ────────────────────────────────────────────────────────\n\
             # Run `make ffi` then `make ui-gen` to wire up the FFI library automatically.\n\
             # add_library({effective_prog}_ffi SHARED IMPORTED)\n\
             # set_target_properties({effective_prog}_ffi PROPERTIES\n\
             #     IMPORTED_LOCATION \"${{CMAKE_CURRENT_SOURCE_DIR}}/../../target/debug/lib{effective_prog}_ffi.so\")\n"
        )
    };

    let ffi_link = if ffi_lib_path.is_some() {
        format!("\x20   {effective_prog}_ffi\n")
    } else {
        format!("\x20   # {effective_prog}_ffi\n")
    };

    let rpath_section = if let Some(path) = ffi_lib_path {
        let ffi_dir = path.rfind('/').map(|i| &path[..i]).unwrap_or(".");
        format!(
            "\nset_target_properties({class}App PROPERTIES\n\
             \x20   BUILD_RPATH \"${{CMAKE_CURRENT_SOURCE_DIR}}/{ffi_dir}\"\n\
             )\n"
        )
    } else {
        String::new()
    };

    format!(
        "cmake_minimum_required(VERSION 3.16)\n\
         project({class} VERSION 1.0 LANGUAGES CXX)\n\
         \n\
         set(CMAKE_CXX_STANDARD 17)\n\
         set(CMAKE_CXX_STANDARD_REQUIRED ON)\n\
         set(CMAKE_AUTOMOC ON)\n\
         \n\
         find_package(Qt6 REQUIRED COMPONENTS Core Gui Widgets Qml Quick QuickWidgets Concurrent)\n\
         {ffi_imported}\n\
         # ── Plugin (loaded by Basecamp) ────────────────────────────────────────────\n\
         add_library({class}Plugin SHARED\n\
         \x20   src/{class}Backend.cpp\n\
         \x20   src/{class}Plugin.cpp\n\
         )\n\
         \n\
         # Resource name \"{res_name}\" must match Q_INIT_RESOURCE({res_name}) in {class}Plugin.cpp\n\
         qt_add_resources({class}Plugin \"{res_name}\"\n\
         \x20   PREFIX \"/\"\n\
         \x20   FILES qml/Main.qml\n\
         )\n\
         \n\
         target_link_libraries({class}Plugin PRIVATE\n\
         \x20   Qt6::Core Qt6::Gui Qt6::Widgets Qt6::Qml Qt6::Quick Qt6::QuickWidgets Qt6::Concurrent\n\
         {ffi_link}\
         )\n\
         \n\
         # Output name must match manifest.json \"main\" value (lib<name>_plugin.so)\n\
         set_target_properties({class}Plugin PROPERTIES OUTPUT_NAME \"{effective_prog}_plugin\")\n\
         \n\
         # ── Standalone preview app ─────────────────────────────────────────────────\n\
         add_executable({class}App\n\
         \x20   src/main.cpp\n\
         \x20   src/{class}Backend.cpp\n\
         \x20   src/{class}Plugin.cpp\n\
         )\n\
         \n\
         # Same resource name as the plugin so Q_INIT_RESOURCE resolves in both targets\n\
         qt_add_resources({class}App \"{res_name}\"\n\
         \x20   PREFIX \"/\"\n\
         \x20   FILES qml/Main.qml\n\
         )\n\
         \n\
         target_link_libraries({class}App PRIVATE\n\
         \x20   Qt6::Core Qt6::Gui Qt6::Widgets Qt6::Qml Qt6::Quick Qt6::QuickWidgets Qt6::Concurrent\n\
         {ffi_link}\
         )\n\
         {rpath_section}"
    )
}

fn gen_manifest_json(idl: &SpelIdl, effective_prog: &str) -> String {
    let desc = format!("Qt/QML Basecamp module for the {} program", idl.name);
    let ver = &idl.version;
    let main_lib = format!("lib{effective_prog}_plugin.so");
    format!(
        "{{\n\
         \x20 \"author\": \"\",\n\
         \x20 \"category\": \"tools\",\n\
         \x20 \"dependencies\": [],\n\
         \x20 \"description\": \"{desc}\",\n\
         \x20 \"icon\": \"\",\n\
         \x20 \"main\": {{\n\
         \x20   \"linux-amd64\": \"{main_lib}\",\n\
         \x20   \"linux-amd64-dev\": \"{main_lib}\",\n\
         \x20   \"linux-x86_64-dev\": \"{main_lib}\"\n\
         \x20 }},\n\
         \x20 \"manifestVersion\": \"0.2.0\",\n\
         \x20 \"name\": \"{effective_prog}\",\n\
         \x20 \"type\": \"ui\",\n\
         \x20 \"version\": \"{ver}\"\n\
         }}\n"
    )
}
