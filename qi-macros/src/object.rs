use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{
    parse::{Parse, ParseStream},
    AttrStyle, Attribute, Error, Expr, ExprLit, Ident, ItemTrait, Lit, LitStr, Meta, MetaNameValue,
    Result, TraitItem, TraitItemFn,
};

#[derive(Debug)]
pub(super) struct Object {
    trait_item: ItemTrait,
    name: String,
    methods: Vec<Method>,
    signals: Vec<Signal>,
    properties: Vec<Property>,
    description: Vec<LitStr>,
}

impl ToTokens for Object {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.trait_item.to_tokens(tokens)
    }
}

impl Parse for Object {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut trait_item = ItemTrait::parse(input)?;

        if !trait_item.generics.params.is_empty() {
            return Err(Error::new_spanned(
                &trait_item.generics,
                "qi object traits cannot be generic",
            ));
        }

        let description = trait_item
            .attrs
            .iter()
            .filter_map(attribute_outer_doc)
            .collect();

        let items_len = trait_item.items.len();
        let mut methods = Vec::with_capacity(items_len);
        let mut signals = Vec::with_capacity(items_len);
        let mut properties = Vec::with_capacity(items_len);

        for item in &mut trait_item.items {
            reject_unknown_qi_attrs(item_attrs(item))?;
            reject_tagged_non_fn(item)?;
            if let Some(method) = Method::from_item(item)? {
                methods.push(method)
            } else if let Some(signal) = Signal::from_item(item)? {
                signals.push(signal)
            } else if let Some(property) = Property::from_item(item)? {
                properties.push(property)
            }
            strip_qi_attrs(item);
        }

        Ok(Self {
            name: trait_item.ident.to_string(),
            trait_item,
            methods,
            signals,
            properties,
            description,
        })
    }
}

#[derive(Debug)]
struct Method {
    func: TraitItemFn,
}

impl Method {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::method").map(|func| func.map(|func| Self { func }))
    }
}

#[derive(Debug)]
struct Signal {
    func: TraitItemFn,
}

impl Signal {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::signal").map(|func| func.map(|func| Self { func }))
    }
}

#[derive(Debug)]
struct Property {
    func: TraitItemFn,
}

impl Property {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::property").map(|func| func.map(|func| Self { func }))
    }
}

fn tagged_fn(item: &TraitItem, tag: &str) -> Result<Option<TraitItemFn>> {
    let func = match item {
        TraitItem::Fn(f) => f,
        _ => return Ok(None),
    };
    let Some(attr) = func
        .attrs
        .iter()
        .find(|attr| is_member_tag_attribute(attr, tag.trim_start_matches("qi::")))
    else {
        return Ok(None);
    };
    parse_name_arg(attr, tag)?;
    require_ref_self(func, tag)?;
    Ok(Some(func.clone()))
}

fn require_ref_self(func: &TraitItemFn, tag: &str) -> Result<()> {
    match func.sig.receiver() {
        Some(recv) if recv.reference.is_some() && recv.mutability.is_none() => Ok(()),
        Some(recv) if recv.mutability.is_some() => Err(Error::new_spanned(
            recv,
            format!("`#[{tag}]` requires a `&self` receiver, not `&mut self`"),
        )),
        Some(recv) => Err(Error::new_spanned(
            recv,
            format!("`#[{tag}]` requires a `&self` receiver, not `self` by value"),
        )),
        None => Err(Error::new_spanned(
            &func.sig,
            format!("`#[{tag}]` requires a `&self` receiver"),
        )),
    }
}

fn parse_name_arg(attr: &Attribute, tag: &str) -> Result<()> {
    match &attr.meta {
        Meta::Path(_) => Ok(()),
        Meta::List(_) => attr.parse_args_with(|input: ParseStream| {
            let key: Ident = input.parse()?;
            if key != "name" {
                return Err(Error::new(
                    key.span(),
                    format!("unknown qi attribute argument `{key}`"),
                ));
            }
            input.parse::<syn::Token![=]>()?;
            match input.parse::<Lit>()? {
                Lit::Str(_) => Ok(()),
                other => Err(Error::new_spanned(other, "`name` must be a string literal")),
            }
        }),
        Meta::NameValue(_) => Err(Error::new_spanned(
            attr,
            format!("`#[{tag}]` takes `name = \"...\"` in parentheses"),
        )),
    }
}

fn reject_tagged_non_fn(item: &TraitItem) -> Result<()> {
    if matches!(item, TraitItem::Fn(_)) {
        return Ok(());
    }
    for attr in item_attrs(item) {
        let Some(leaf) = qi_leaf(attr) else {
            continue;
        };
        if leaf == "method" || leaf == "property" || leaf == "signal" {
            return Err(Error::new_spanned(
                attr,
                format!("`#[qi::{leaf}]` requires a function"),
            ));
        }
    }
    Ok(())
}

fn reject_unknown_qi_attrs(attrs: &[Attribute]) -> Result<()> {
    for attr in attrs {
        let Some(leaf) = qi_leaf(attr) else {
            continue;
        };
        if leaf != "method" && leaf != "property" && leaf != "signal" {
            return Err(Error::new_spanned(
                attr,
                format!("unknown qi attribute `{leaf}`"),
            ));
        }
    }
    Ok(())
}

fn strip_qi_attrs(item: &mut TraitItem) {
    let attrs = match item {
        TraitItem::Const(item) => &mut item.attrs,
        TraitItem::Fn(item) => &mut item.attrs,
        TraitItem::Type(item) => &mut item.attrs,
        TraitItem::Macro(item) => &mut item.attrs,
        _ => return,
    };
    attrs.retain(|attr| qi_leaf(attr).is_none());
}

fn qi_leaf(attr: &Attribute) -> Option<&Ident> {
    if attr.style != AttrStyle::Outer {
        return None;
    }
    let mut segments = attr.path().segments.iter();
    match (segments.next(), segments.next(), segments.next()) {
        (Some(first), Some(second), None) if first.ident == "qi" => Some(&second.ident),
        _ => None,
    }
}

fn item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

fn attribute_outer_doc(attr: &Attribute) -> Option<LitStr> {
    if attr.style != AttrStyle::Outer {
        return None;
    }
    let MetaNameValue { path, value, .. } = attr.meta.require_name_value().ok()?;
    if !path.is_ident("doc") {
        return None;
    }
    let doc_str = match value {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => s,
        _ => return None,
    };

    Some(doc_str.clone())
}

fn is_member_tag_attribute(attr: &Attribute, ty: &str) -> bool {
    qi_leaf(attr).is_some_and(|leaf| leaf == ty)
}
