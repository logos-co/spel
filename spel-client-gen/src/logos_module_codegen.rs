//! Qt/QML Logos Basecamp module scaffold generation from SPEL IDL.
//!
//! Generates: XyzBackend.h/.cpp, XyzPlugin.h/.cpp, qml/Main.qml,
//!            module.yaml, metadata.json

use spel_framework_core::idl::*;
use std::collections::HashSet;
use crate::util::*;

pub struct LogosModuleOutput {
    pub backend_h: String,
    pub backend_cpp: String,
    pub plugin_h: String,
    pub plugin_cpp: String,
    pub main_qml: String,
    pub module_yaml: String,
    pub metadata_json: String,
}

pub fn generate_logos_module(idl: &SpelIdl) -> Result<LogosModuleOutput, String> {
    let class = pascal_case(&idl.name);
    let prog = snake_case(&idl.name);
    let fetches = fetch_eligible_accounts(idl);

    Ok(LogosModuleOutput {
        backend_h:     gen_backend_h(idl, &class, &prog, &fetches),
        backend_cpp:   gen_backend_cpp(idl, &class, &prog, &fetches),
        plugin_h:      gen_plugin_h(&class),
        plugin_cpp:    gen_plugin_cpp(&class),
        main_qml:      gen_main_qml(idl, &fetches),
        module_yaml:   gen_module_yaml(idl, &prog, &class),
        metadata_json: gen_metadata_json(idl, &prog),
    })
}

// ── Type helpers ──────────────────────────────────────────────────────────────

fn qt_type(ty: &IdlType) -> (String, bool) {
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "u8" | "u16" | "u32" => ("quint32".into(), false),
            "u64" => ("quint64".into(), false),
            "i8" | "i16" | "i32" => ("qint32".into(), false),
            "i64" => ("qint64".into(), false),
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
        IdlType::Option { option } => qt_type(option),
        IdlType::Defined { .. } => ("QVariantMap".into(), true),
        IdlType::Array { array: (elem, _) } => match elem.as_ref() {
            IdlType::Primitive(p) if p == "u8" => ("QString".into(), true),
            _ => ("QVariantList".into(), true),
        },
    }
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

fn is_bool_type(ty: &IdlType) -> bool {
    matches!(ty, IdlType::Primitive(p) if p == "bool")
}

// ── Fetch-eligible account analysis ──────────────────────────────────────────

struct FetchAccount {
    acc_name: String,
    seed_params: Vec<(String, IdlType)>, // (idl_name, idl_type) for each Arg seed
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

            let has_type = idl.accounts.iter().any(|at| {
                snake_case(&at.name) == acc_name
                    && at.type_.kind == "struct"
                    && !at.type_.fields.is_empty()
            });
            if !has_type {
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

fn arg_to_json_lines(ty: &IdlType, qt_name: &str, json_key: &str) -> Vec<String> {
    match ty {
        IdlType::Primitive(p) => match p.as_str() {
            "u8" | "u16" | "u32" | "i8" | "i16" | "i32" => vec![
                format!("    args[\"{json_key}\"] = static_cast<int>({qt_name});"),
            ],
            "u64" | "i64" => vec![
                format!("    args[\"{json_key}\"] = static_cast<qint64>({qt_name});"),
            ],
            _ => vec![format!("    args[\"{json_key}\"] = {qt_name};")],
        },
        IdlType::Vec { .. } => vec![
            "    {".to_string(),
            format!("        QJsonArray _arr;"),
            format!("        for (const auto& _s : {qt_name}) _arr.append(QString(_s));"),
            format!("        args[\"{json_key}\"] = _arr;"),
            "    }".to_string(),
        ],
        _ => vec![format!("    args[\"{json_key}\"] = {qt_name};")],
    }
}

fn param_to_json_lines(p: &InstrParam) -> Vec<String> {
    match &p.kind {
        ParamKind::Account => vec![format!("    args[\"{}\"] = {};", p.idl_key, p.qt_name)],
        ParamKind::Arg(ty) => arg_to_json_lines(ty, &p.qt_name, &p.idl_key),
    }
}

fn qml_field_expr(kind: &ParamKind, field_id: &str) -> String {
    match kind {
        ParamKind::Account => format!("{field_id}.text"),
        ParamKind::Arg(ty) => match ty {
            IdlType::Primitive(p) => match p.as_str() {
                "bool" => format!("{field_id}.checked"),
                "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" => {
                    format!("parseInt({field_id}.text)")
                }
                _ => format!("{field_id}.text"),
            },
            _ => format!("{field_id}.text"),
        },
    }
}

// ── Backend.h ─────────────────────────────────────────────────────────────────

fn gen_backend_h(idl: &SpelIdl, class: &str, _prog: &str, fetches: &[FetchAccount]) -> String {
    let mut o = String::new();
    let backend = format!("{class}Backend");

    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str("#pragma once\n\n");
    o.push_str("#include <functional>\n");
    o.push_str("#include <QFutureWatcher>\n");
    o.push_str("#include <QJsonArray>\n");
    o.push_str("#include <QJsonObject>\n");
    o.push_str("#include <QObject>\n");
    o.push_str("#include <QString>\n");
    o.push_str("#include <QStringList>\n");
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
    o.push_str("    Q_PROPERTY(bool    busy       READ busy       NOTIFY busyChanged)\n");
    o.push_str("    Q_PROPERTY(QString lastError  READ lastError  NOTIFY lastErrorChanged)\n");
    o.push_str("    Q_PROPERTY(QString lastTxHash READ lastTxHash NOTIFY lastTxHashChanged)\n\n");

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

    o.push_str("    bool    busy()       const { return m_busy; }\n");
    o.push_str("    QString lastError()  const { return m_lastError; }\n");
    o.push_str("    QString lastTxHash() const { return m_lastTxHash; }\n\n");

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

    o.push_str("signals:\n");
    for f in fetches {
        let p = camel_case(&f.acc_name);
        o.push_str(&format!("    void {p}Changed();\n"));
    }
    o.push_str("    void busyChanged();\n");
    o.push_str("    void lastErrorChanged();\n");
    o.push_str("    void lastTxHashChanged();\n");
    o.push_str("    void operationSuccess(const QString& operation, const QString& txHash);\n");
    o.push_str("    void operationError(const QString& operation, const QString& error);\n\n");

    o.push_str("private:\n");
    o.push_str("    using FfiFn = char* (*)(const char*);\n\n");
    o.push_str("    void        dispatchFfi(const QString& operation, std::function<QString()> fn);\n");
    o.push_str("    void        handleFfiResult(const QString& operation, const QString& result);\n");
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
    o.push_str("    bool    m_busy      = false;\n");
    o.push_str("    QString m_lastError;\n");
    o.push_str("    QString m_lastTxHash;\n");
    o.push_str("};\n");

    o
}

// ── Backend.cpp ───────────────────────────────────────────────────────────────

fn gen_backend_cpp(
    idl: &SpelIdl,
    class: &str,
    prog: &str,
    fetches: &[FetchAccount],
) -> String {
    let mut o = String::new();
    let backend = format!("{class}Backend");
    let prog_env = prog.to_uppercase();

    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str(&format!("#include \"{backend}.h\"\n\n"));
    o.push_str("#include <QJsonArray>\n");
    o.push_str("#include <QJsonDocument>\n");
    o.push_str("#include <QJsonObject>\n");
    o.push_str("#include <QMetaObject>\n");
    o.push_str("#include <QThreadPool>\n");
    o.push_str("#include <QtConcurrent/QtConcurrent>\n\n");

    // extern "C" block
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
    o.push_str(&format!("    void  {prog}_free_string(char* s);\n"));
    o.push_str("}\n\n");

    // Constructor
    o.push_str("// ── Construction ──────────────────────────────────────────────────────────\n\n");
    o.push_str(&format!("{backend}::{backend}(LogosAPI* /*api*/, QObject* parent)\n"));
    o.push_str("    : QObject(parent)\n");
    o.push_str("    , m_walletPath(qEnvironmentVariable(\"NSSA_WALLET_HOME_DIR\", \".scaffold/wallet\"))\n");
    o.push_str("    , m_sequencerUrl(qEnvironmentVariable(\"NSSA_SEQUENCER_URL\", \"http://127.0.0.1:3040\"))\n");
    o.push_str(&format!(
        "    , m_programIdHex(qEnvironmentVariable(\"{prog_env}_PROGRAM_ID_HEX\"))\n"
    ));
    o.push_str("{}\n\n");
    o.push_str(&format!("{backend}::~{backend}() = default;\n\n"));

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
    o.push_str("    if (!raw) return R\"({\"success\":false,\"error\":\"null return from FFI\"})\";\n");
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
    o.push_str("    if (obj.contains(\"tx_hash\")) {\n");
    o.push_str("        m_lastTxHash = obj.value(\"tx_hash\").toString();\n");
    o.push_str("        emit lastTxHashChanged();\n");
    o.push_str("        emit operationSuccess(operation, m_lastTxHash);\n");
    o.push_str("    }\n}\n\n");

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
            o.push_str("            QJsonObject obj = QJsonDocument::fromJson(result.toUtf8()).object();\n");
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

    o
}

// ── Plugin.h ──────────────────────────────────────────────────────────────────

fn gen_plugin_h(class: &str) -> String {
    format!(
        "// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n\
         #pragma once\n\n\
         #include <QQmlExtensionPlugin>\n\n\
         class {class}Plugin : public QQmlExtensionPlugin {{\n\
         \tQ_OBJECT\n\
         \tQ_PLUGIN_METADATA(IID QQmlExtensionInterface_iid)\n\n\
         public:\n\
         \tvoid registerTypes(const char* uri) override;\n\
         }};\n"
    )
}

// ── Plugin.cpp ────────────────────────────────────────────────────────────────

fn gen_plugin_cpp(class: &str) -> String {
    format!(
        "// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n\
         #include \"{class}Plugin.h\"\n\
         #include \"{class}Backend.h\"\n\n\
         #include <QQmlEngine>\n\n\
         void {class}Plugin::registerTypes(const char* uri) {{\n\
         \tqmlRegisterSingletonType<{class}Backend>(uri, 1, 0, \"Backend\",\n\
         \t\t[](QQmlEngine* engine, QJSEngine*) -> QObject* {{\n\
         \t\t\treturn new {class}Backend(nullptr, engine);\n\
         \t\t}}\n\
         \t);\n\
         }}\n"
    )
}

// ── Main.qml ─────────────────────────────────────────────────────────────────

fn gen_main_qml(idl: &SpelIdl, fetches: &[FetchAccount]) -> String {
    let mut o = String::new();

    o.push_str("// Auto-generated by spel-client-gen --target logos-module. DO NOT EDIT.\n");
    o.push_str("import QtQuick 2.15\n");
    o.push_str("import QtQuick.Controls 2.15\n");
    o.push_str("import QtQuick.Layouts 1.15\n\n");

    o.push_str("Item {\n");
    o.push_str("    id: root\n\n");

    o.push_str("    // ── Logos palette ─────────────────────────────────────────────────────\n");
    o.push_str("    readonly property color colBg:      \"#0f1117\"\n");
    o.push_str("    readonly property color colSurface: \"#1a1d27\"\n");
    o.push_str("    readonly property color colBorder:  \"#2d3148\"\n");
    o.push_str("    readonly property color colPrimary: \"#7c6ef5\"\n");
    o.push_str("    readonly property color colSuccess: \"#3ecf8e\"\n");
    o.push_str("    readonly property color colError:   \"#e05252\"\n");
    o.push_str("    readonly property color colText:    \"#e8e9f0\"\n");
    o.push_str("    readonly property color colMuted:   \"#6b7280\"\n");
    o.push_str("    readonly property int   radius:     12\n\n");

    o.push_str("    Connections {\n");
    o.push_str("        target: backend\n");
    o.push_str("        function onOperationSuccess(operation, txHash) {\n");
    o.push_str("            toast.show(\"\\u2713 \" + operation + \" \\u00b7 \" + txHash.slice(0, 12) + \"\\u2026\", root.colSuccess)\n");
    o.push_str("        }\n");
    o.push_str("        function onOperationError(operation, error) {\n");
    o.push_str("            toast.show(\"\\u2717 \" + operation + \": \" + error, root.colError)\n");
    o.push_str("        }\n");
    o.push_str("    }\n\n");

    o.push_str("    Rectangle {\n");
    o.push_str("        anchors.fill: parent\n");
    o.push_str("        color: root.colBg\n\n");

    o.push_str("        ScrollView {\n");
    o.push_str("            id: scrollView\n");
    o.push_str("            anchors.fill: parent\n");
    o.push_str("            clip: true\n\n");

    o.push_str("            ColumnLayout {\n");
    o.push_str("                width: scrollView.width\n");
    o.push_str("                spacing: 12\n");
    o.push_str("                leftPadding: 16; rightPadding: 16\n");
    o.push_str("                topPadding: 16; bottomPadding: 80\n\n");

    o.push_str("                BusyIndicator {\n");
    o.push_str("                    running: backend.busy\n");
    o.push_str("                    visible: running\n");
    o.push_str("                    Layout.alignment: Qt.AlignHCenter\n");
    o.push_str("                }\n\n");

    // State sections first (read before write)
    for f in fetches {
        qml_state_section(&mut o, f);
    }

    // Instruction sections
    for ix in &idl.instructions {
        qml_instruction_section(&mut o, ix);
    }

    o.push_str("            }\n"); // ColumnLayout
    o.push_str("        }\n\n"); // ScrollView

    qml_toast(&mut o);

    o.push_str("    }\n"); // Rectangle
    o.push_str("}\n"); // Item

    o
}

fn qml_textfield(o: &mut String, id: &str, placeholder: &str, indent: &str) {
    o.push_str(&format!("{indent}TextField {{\n"));
    o.push_str(&format!("{indent}    id: {id}\n"));
    o.push_str(&format!("{indent}    Layout.fillWidth: true\n"));
    o.push_str(&format!(
        "{indent}    placeholderText: \"{placeholder}\"\n"
    ));
    o.push_str(&format!("{indent}    color: root.colText\n"));
    o.push_str(&format!(
        "{indent}    placeholderTextColor: root.colMuted\n"
    ));
    o.push_str(&format!("{indent}    background: Rectangle {{\n"));
    o.push_str(&format!("{indent}        color: root.colBg\n"));
    o.push_str(&format!(
        "{indent}        border.color: root.colBorder\n"
    ));
    o.push_str(&format!(
        "{indent}        radius: root.radius / 2\n"
    ));
    o.push_str(&format!("{indent}    }}\n"));
    o.push_str(&format!("{indent}}}\n\n"));
}

fn qml_instruction_section(o: &mut String, ix: &IdlInstruction) {
    let params = instruction_params(ix);
    let col_id = format!("col_{}", snake_case(&ix.name));
    let method = camel_case(&ix.name);
    let title = pascal_case(&ix.name);
    let ind = "                ";

    o.push_str(&format!(
        "{ind}// ── {title} ──────────────────────────────────────────────\n"
    ));
    o.push_str(&format!("{ind}Rectangle {{\n"));
    o.push_str(&format!("{ind}    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}    color: root.colSurface\n"));
    o.push_str(&format!("{ind}    radius: root.radius\n"));
    o.push_str(&format!(
        "{ind}    implicitHeight: {col_id}.implicitHeight + 32\n\n"
    ));
    o.push_str(&format!("{ind}    ColumnLayout {{\n"));
    o.push_str(&format!("{ind}        id: {col_id}\n"));
    o.push_str(&format!(
        "{ind}        anchors {{ left: parent.left; right: parent.right; margins: 16 }}\n"
    ));
    o.push_str(&format!("{ind}        y: 16; spacing: 8\n\n"));
    o.push_str(&format!("{ind}        Text {{\n"));
    o.push_str(&format!("{ind}            text: \"{title}\"\n"));
    o.push_str(&format!("{ind}            color: root.colText\n"));
    o.push_str(&format!(
        "{ind}            font.pixelSize: 14; font.bold: true\n"
    ));
    o.push_str(&format!("{ind}        }}\n\n"));

    for p in &params {
        let field_id = format!(
            "{}_{}_f",
            snake_case(&ix.name),
            snake_case(&p.qt_name)
        );
        if matches!(&p.kind, ParamKind::Arg(ty) if is_bool_type(ty)) {
            o.push_str(&format!("{ind}        RowLayout {{\n"));
            o.push_str(&format!("{ind}            Layout.fillWidth: true\n"));
            o.push_str(&format!("{ind}            CheckBox {{\n"));
            o.push_str(&format!("{ind}                id: {field_id}\n"));
            o.push_str(&format!("{ind}                checked: false\n"));
            o.push_str(&format!("{ind}            }}\n"));
            o.push_str(&format!("{ind}            Text {{\n"));
            o.push_str(&format!(
                "{ind}                text: \"{}\"\n",
                p.qt_name
            ));
            o.push_str(&format!("{ind}                color: root.colText\n"));
            o.push_str(&format!("{ind}                font.pixelSize: 13\n"));
            o.push_str(&format!("{ind}            }}\n"));
            o.push_str(&format!("{ind}        }}\n\n"));
        } else {
            qml_textfield(o, &field_id, &p.qt_name, &format!("{ind}        "));
        }
    }

    let call_args = params
        .iter()
        .map(|p| {
            let fid = format!(
                "{}_{}_f",
                snake_case(&ix.name),
                snake_case(&p.qt_name)
            );
            qml_field_expr(&p.kind, &fid)
        })
        .collect::<Vec<_>>()
        .join(", ");

    o.push_str(&format!("{ind}        Button {{\n"));
    o.push_str(&format!(
        "{ind}            text: backend.busy ? \"\\u2026\" : \"{title}\"\n"
    ));
    o.push_str(&format!("{ind}            enabled: !backend.busy\n"));
    o.push_str(&format!(
        "{ind}            Layout.alignment: Qt.AlignRight\n"
    ));
    o.push_str(&format!(
        "{ind}            onClicked: backend.{method}({call_args})\n"
    ));
    o.push_str(&format!("{ind}            background: Rectangle {{\n"));
    o.push_str(&format!("{ind}                color: parent.down ? Qt.darker(root.colPrimary, 1.2) : root.colPrimary\n"));
    o.push_str(&format!(
        "{ind}                radius: root.radius / 2\n"
    ));
    o.push_str(&format!(
        "{ind}                opacity: parent.enabled ? 1.0 : 0.5\n"
    ));
    o.push_str(&format!("{ind}            }}\n"));
    o.push_str(&format!("{ind}            contentItem: Text {{\n"));
    o.push_str(&format!("{ind}                text: parent.text\n"));
    o.push_str(&format!("{ind}                color: root.colText\n"));
    o.push_str(&format!(
        "{ind}                horizontalAlignment: Text.AlignHCenter\n"
    ));
    o.push_str(&format!(
        "{ind}                verticalAlignment: Text.AlignVCenter\n"
    ));
    o.push_str(&format!("{ind}            }}\n"));
    o.push_str(&format!("{ind}        }}\n"));
    o.push_str(&format!("{ind}    }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}}}\n\n")); // Rectangle
}

fn qml_state_section(o: &mut String, f: &FetchAccount) {
    let prop = camel_case(&f.acc_name);
    let title = pascal_case(&f.acc_name);
    let col_id = format!("colFetch{title}");
    let fetch_method = format!("fetch{title}");
    let ind = "                ";

    o.push_str(&format!(
        "{ind}// ── {title} State ─────────────────────────────────────────────\n"
    ));
    o.push_str(&format!("{ind}Rectangle {{\n"));
    o.push_str(&format!("{ind}    Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}    color: root.colSurface\n"));
    o.push_str(&format!("{ind}    radius: root.radius\n"));
    o.push_str(&format!(
        "{ind}    implicitHeight: {col_id}.implicitHeight + 32\n\n"
    ));
    o.push_str(&format!("{ind}    ColumnLayout {{\n"));
    o.push_str(&format!("{ind}        id: {col_id}\n"));
    o.push_str(&format!(
        "{ind}        anchors {{ left: parent.left; right: parent.right; margins: 16 }}\n"
    ));
    o.push_str(&format!("{ind}        y: 16; spacing: 8\n\n"));

    // Seed input fields
    for (name, _ty) in &f.seed_params {
        let fid = format!("fetch{}_{}_f", title, snake_case(name));
        let label = format!("{} (seed)", camel_case(name));
        qml_textfield(o, &fid, &label, &format!("{ind}        "));
    }

    // Header row: title + refresh button
    let seed_call = f
        .seed_params
        .iter()
        .map(|(name, ty)| {
            let fid = format!("fetch{}_{}_f", title, snake_case(name));
            match ty {
                IdlType::Primitive(p) => match p.as_str() {
                    "bool" => format!("{fid}.checked"),
                    "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" => {
                        format!("parseInt({fid}.text)")
                    }
                    _ => format!("{fid}.text"),
                },
                _ => format!("{fid}.text"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    o.push_str(&format!("{ind}        RowLayout {{\n"));
    o.push_str(&format!("{ind}            Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}            Text {{\n"));
    o.push_str(&format!(
        "{ind}                text: \"{title} State\"\n"
    ));
    o.push_str(&format!("{ind}                color: root.colText\n"));
    o.push_str(&format!(
        "{ind}                font.pixelSize: 14; font.bold: true\n"
    ));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}            }}\n"));
    o.push_str(&format!("{ind}            Button {{\n"));
    o.push_str(&format!("{ind}                text: \"\\u21ba\"\n")); // ↺
    o.push_str(&format!(
        "{ind}                onClicked: backend.{fetch_method}({seed_call})\n"
    ));
    o.push_str(&format!("{ind}                background: Rectangle {{\n"));
    o.push_str(&format!(
        "{ind}                    color: root.colSurface\n"
    ));
    o.push_str(&format!(
        "{ind}                    border.color: root.colBorder\n"
    ));
    o.push_str(&format!(
        "{ind}                    radius: root.radius / 2\n"
    ));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!(
        "{ind}                contentItem: Text {{\n"
    ));
    o.push_str(&format!("{ind}                    text: parent.text\n"));
    o.push_str(&format!(
        "{ind}                    color: root.colMuted\n"
    ));
    o.push_str(&format!(
        "{ind}                    horizontalAlignment: Text.AlignHCenter\n"
    ));
    o.push_str(&format!(
        "{ind}                    verticalAlignment: Text.AlignVCenter\n"
    ));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n"));
    o.push_str(&format!("{ind}        }}\n\n")); // RowLayout

    // Key-value display
    o.push_str(&format!("{ind}        Repeater {{\n"));
    o.push_str(&format!(
        "{ind}            model: Object.keys(backend.{prop})\n"
    ));
    o.push_str(&format!("{ind}            delegate: RowLayout {{\n"));
    o.push_str(&format!("{ind}                Layout.fillWidth: true\n"));
    o.push_str(&format!("{ind}                Text {{\n"));
    o.push_str(&format!("{ind}                    text: modelData + \":\"\n"));
    o.push_str(&format!(
        "{ind}                    color: root.colMuted\n"
    ));
    o.push_str(&format!(
        "{ind}                    font.pixelSize: 12\n"
    ));
    o.push_str(&format!(
        "{ind}                    Layout.preferredWidth: 140\n"
    ));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}                Text {{\n"));
    o.push_str(&format!(
        "{ind}                    text: backend.{prop}[modelData] ?? \"\"\n"
    ));
    o.push_str(&format!("{ind}                    color: root.colText\n"));
    o.push_str(&format!(
        "{ind}                    font.pixelSize: 12\n"
    ));
    o.push_str(&format!(
        "{ind}                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere\n"
    ));
    o.push_str(&format!(
        "{ind}                    Layout.fillWidth: true\n"
    ));
    o.push_str(&format!("{ind}                }}\n"));
    o.push_str(&format!("{ind}            }}\n"));
    o.push_str(&format!("{ind}        }}\n\n")); // Repeater

    o.push_str(&format!("{ind}        Text {{\n"));
    o.push_str(&format!(
        "{ind}            visible: Object.keys(backend.{prop}).length === 0\n"
    ));
    o.push_str(&format!(
        "{ind}            text: \"No data. Press \\u21ba to fetch.\"\n"
    ));
    o.push_str(&format!("{ind}            color: root.colMuted\n"));
    o.push_str(&format!("{ind}            font.pixelSize: 12\n"));
    o.push_str(&format!("{ind}        }}\n"));
    o.push_str(&format!("{ind}    }}\n")); // ColumnLayout
    o.push_str(&format!("{ind}}}\n\n")); // Rectangle
}

fn qml_toast(o: &mut String) {
    o.push_str("        Rectangle {\n");
    o.push_str("            id: toast\n");
    o.push_str("            anchors { bottom: parent.bottom; horizontalCenter: parent.horizontalCenter; bottomMargin: 24 }\n");
    o.push_str("            width: toastText.implicitWidth + 32; height: 40\n");
    o.push_str("            radius: root.radius\n");
    o.push_str("            color: root.colSurface\n");
    o.push_str("            opacity: 0; visible: opacity > 0\n\n");
    o.push_str("            function show(msg, col) {\n");
    o.push_str("                toastText.text = msg\n");
    o.push_str("                toast.color = col\n");
    o.push_str("                toast.opacity = 1\n");
    o.push_str("                toastTimer.restart()\n");
    o.push_str("            }\n\n");
    o.push_str("            Text {\n");
    o.push_str("                id: toastText\n");
    o.push_str("                anchors.centerIn: parent\n");
    o.push_str("                color: root.colText\n");
    o.push_str("                font.pixelSize: 13\n");
    o.push_str("            }\n\n");
    o.push_str("            Behavior on opacity { NumberAnimation { duration: 200 } }\n\n");
    o.push_str("            Timer {\n");
    o.push_str("                id: toastTimer\n");
    o.push_str("                interval: 3000\n");
    o.push_str("                onTriggered: toast.opacity = 0\n");
    o.push_str("            }\n");
    o.push_str("        }\n");
}

// ── module.yaml ───────────────────────────────────────────────────────────────

fn gen_module_yaml(idl: &SpelIdl, prog: &str, class: &str) -> String {
    let desc = format!("Qt/QML Basecamp module for the {} program", idl.name);
    let ver = &idl.version;
    let ffi = format!("{prog}_ffi");
    format!(
        "# Auto-generated by spel-client-gen --target logos-module.\n\
         name: {prog}\n\
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

// ── metadata.json ─────────────────────────────────────────────────────────────

fn gen_metadata_json(idl: &SpelIdl, prog: &str) -> String {
    let desc = format!("Qt/QML Basecamp module for the {} program", idl.name);
    let ver = &idl.version;
    let ffi = format!("{prog}_ffi");
    let main_lib = format!("lib{prog}_plugin");
    format!(
        "{{\n\
         \x20 \"name\": \"{prog}\",\n\
         \x20 \"version\": \"{ver}\",\n\
         \x20 \"description\": \"{desc}\",\n\
         \x20 \"type\": \"ui\",\n\
         \x20 \"category\": \"tools\",\n\
         \x20 \"main\": \"{main_lib}\",\n\
         \x20 \"view\": \"qml/Main.qml\",\n\
         \x20 \"nix\": {{\n\
         \x20   \"packages\": {{\n\
         \x20     \"runtime\": [\"qt6.qtdeclarative\", \"qt6.qtwayland\"]\n\
         \x20   }},\n\
         \x20   \"external_libraries\": [\n\
         \x20     {{\n\
         \x20       \"name\": \"{ffi}\",\n\
         \x20       \"vendor_path\": \"lib\"\n\
         \x20     }}\n\
         \x20   ]\n\
         \x20 }}\n\
         }}\n"
    )
}
