use hirsel_intern::Symbol;
use hirsel_span::{Anchor, Span};
use hirsel_syntax as ast;
use hirsel_syntax::DefPath;
use hirsel_tir::*;
use hirsel_types::{Type, TypeInterner};
pub fn syntax_fixture(width: usize) -> ast::ItemSyntax {
    let origin = ast::Origin {
        span: Span::default(),
        tokens: Default::default(),
    };
    let path = ast::Path {
        package: Some(ast::PackageId {
            name: "source".into(),
            version: "1".into(),
        }),
        segments: vec![Symbol::intern("nested"); width],
    };
    let signature = ast::FunctionSyntax {
        attributes: vec![ast::Attribute {
            name: Symbol::intern("state"),
            origin,
            arguments: vec![
                ast::AttributeArgument::StatePromise(ast::StatePromiseSyntax {
                    kind: ast::StatePromiseKind::PreserveCaptures,
                    selectors: vec![path.clone(); width],
                    origin,
                }),
                ast::AttributeArgument::String("payload".repeat(width)),
            ],
        }],
        generics: vec![ast::GenericParam {
            name: Symbol::intern("T"),
            kind: ast::GenericKind::Implementation {
                signature: path.clone(),
                sharing: Vec::with_capacity(width),
            },
        }],
        uses: Some(vec![path.clone(); width]),
        ..Default::default()
    };
    ast::ItemSyntax {
        retention: Default::default(),
        item: ast::Item {
            name_span: origin.span,
            origin,
            name: Symbol::intern("main"),
            definition: ast::DefPath::core("source.memory.main"),
            visibility: ast::Visibility::Private,
            attributes: signature.attributes.clone(),
            kind: ast::ItemKind::Function {
                signature: signature.clone(),
                body: Some(ast::ExprId(0)),
                abi: Some(ast::ForeignAbi {
                    literal: "\"C\"".into(),
                    contract: Some("contract".repeat(width)),
                }),
            },
            implementation_bindings: Some(vec![
                (
                    Symbol::intern("I"),
                    ast::DefPath::core("source.binding")
                );
                width
            ]),
        },
        tree: ast::SyntaxTree {
            expressions: vec![
                ast::Expr {
                    origin,
                    kind: ast::ExprKind::Closure {
                        signature: signature.clone(),
                        body: ast::ExprId(0),
                    },
                },
                ast::Expr {
                    origin,
                    kind: ast::ExprKind::Literal(ast::Literal::String("literal".repeat(width))),
                },
                ast::Expr {
                    origin,
                    kind: ast::ExprKind::Call {
                        callee: ast::ExprId(0),
                        arguments: Vec::with_capacity(width),
                    },
                },
            ],
            statements: vec![ast::Stmt {
                origin,
                kind: ast::StmtKind::Function {
                    name: Symbol::intern("inner"),
                    signature: signature.clone(),
                    body: ast::ExprId(0),
                },
            }],
            patterns: vec![ast::Pattern {
                origin,
                kind: ast::PatternKind::Constructor {
                    path: path.clone(),
                    fields: Vec::with_capacity(width),
                    rest: false,
                },
            }],
            types: vec![ast::TypeExpr {
                origin,
                kind: ast::TypeExprKind::Qualified {
                    base: ast::TypeExprId(0),
                    modifiers: vec![ast::TypeModifier::Unique; width],
                    brand: Some(ast::BrandSyntax {
                        sibling: true,
                        path: path.clone(),
                    }),
                    relation: Some(ast::RelationSyntax {
                        places: vec![ast::RelationPlace::Path(path); width],
                        origin,
                    }),
                },
            }],
            formats: vec![ast::FormatString {
                expression: ast::ExprId(0),
                pieces: vec!["piece".repeat(width)],
                holes: vec![ast::ExprId(0)],
                specs: vec![":>8".into()],
            }],
            analyses: vec![ast::AnalysisAnnotation {
                site: ast::AnalysisSite::Item,
                name: "analysis".into(),
                payload: vec!["value".repeat(width)],
                spaced: vec![true; width],
                origin,
            }],
            ..Default::default()
        },
        anchor: Anchor(0),
        text_hash: Default::default(),
        occurrence: 0,
    }
}
pub fn definition() -> Definition {
    let types = TypeInterner::new(None);
    let unit = types.intern(Type::unit());
    Definition {
        caller_bodies: vec![],
        protocol_associations: vec![],
        types: vec![unit].into(),
        path: hirsel_syntax::DefId::intern(&DefPath::core("held")),
        signature_hash: Default::default(),
        bodies: vec![Body {
            arena_scope_grant: None,
            scoped_parameters: Vec::new(),
            parameter_scope: hirsel_types::LexicalOwner::first(&DefPath::core("held"))
                .site(hirsel_types::NameSite::Item),
            locals: vec![],
            places: vec![],
            scopes: vec![],
            blocks: vec![],
            rvalues: Vec::with_capacity(8),
            cleanups: vec![],
            cleanup_nodes: vec![],
            entry: BlockId(0),
            parameters: vec![],
            result: unit,
            borrow_cursors: vec![],
            tail_sites: vec![],
            matches: vec![],
        }],
        entry: BodyId(0),
    }
}
