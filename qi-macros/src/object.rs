use convert_case::{Case, Casing};
use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, ParseStream},
    AttrStyle, Attribute, Error, Expr, ExprLit, FnArg, Ident, ItemTrait, Lit, LitStr, Meta,
    MetaNameValue, Pat, Result, ReturnType, TraitItem, TraitItemFn, Type,
};

pub(super) struct Object {
    trait_item: ItemTrait,
    members: Vec<Member>,
}

impl ToTokens for Object {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        self.trait_item.to_tokens(tokens);
        self.meta_object().to_tokens(tokens);
    }
}

impl Object {
    fn meta_object(&self) -> TokenStream {
        let vis = &self.trait_item.vis;
        let ident = meta_object_ident(&self.trait_item.ident);
        let members = self.members.iter().map(Member::to_add_tokens);
        quote! {
            #vis static #ident: ::once_cell::sync::Lazy<::qi::object::MetaObject> =
                ::once_cell::sync::Lazy::new(|| {
                    let mut builder = ::qi::object::MetaObject::builder();
                    let mut action_id = ::qi::object::ACTION_START_ID;
                    #(#members)*
                    builder.build()
                });
        }
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

        let mut members = Vec::with_capacity(trait_item.items.len());
        for item in &mut trait_item.items {
            reject_unknown_qi_attrs(item_attrs(item))?;
            reject_tagged_non_fn(item)?;
            if let Some(method) = Method::from_item(item)? {
                members.push(Member::Method(method));
            } else if let Some(signal) = Signal::from_item(item)? {
                members.push(Member::Signal(signal));
            } else if let Some(property) = Property::from_item(item)? {
                members.push(Member::Property(property));
            }
            strip_qi_attrs(item);
        }

        Ok(Self {
            trait_item,
            members,
        })
    }
}

enum Member {
    Method(Method),
    Signal(Signal),
    Property(Property),
}

impl Member {
    fn to_add_tokens(&self) -> TokenStream {
        match self {
            Self::Method(method) => method.to_add_tokens(),
            Self::Signal(signal) => signal.to_add_tokens(),
            Self::Property(property) => property.to_add_tokens(),
        }
    }
}

struct Method {
    func: TraitItemFn,
    name: String,
}

impl Method {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::method").map(|tagged| tagged.map(|(func, name)| Self { func, name }))
    }

    fn to_add_tokens(&self) -> TokenStream {
        let name = &self.name;
        let description = member_docs(&self.func);
        let set_description = description.map(|description| {
            quote! {
                method.set_description(#description);
            }
        });
        let parameters = method_parameters(&self.func);
        let return_ty = method_return_type(&self.func);
        quote! {
            builder.add_method({
                let uid = action_id.wrapping_next();
                let mut method = ::qi::object::MetaMethod::builder(uid);
                method.set_name(#name);
                #set_description
                #parameters
                method.return_value().set_type(<#return_ty as ::qi::value::Reflect>::ty());
                method.build()
            });
        }
    }
}

struct Signal {
    func: TraitItemFn,
    name: String,
}

impl Signal {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::signal").map(|tagged| tagged.map(|(func, name)| Self { func, name }))
    }

    fn to_add_tokens(&self) -> TokenStream {
        let name = &self.name;
        let ty = payload_type(&self.func);
        quote! {
            builder.add_signal({
                let uid = action_id.wrapping_next();
                ::qi::object::MetaSignal {
                    uid,
                    name: ::std::string::String::from(#name),
                    signature: <#ty as ::qi::value::Reflect>::signature(),
                }
            });
        }
    }
}

struct Property {
    func: TraitItemFn,
    name: String,
}

impl Property {
    fn from_item(item: &TraitItem) -> Result<Option<Self>> {
        tagged_fn(item, "qi::property").map(|tagged| tagged.map(|(func, name)| Self { func, name }))
    }

    fn to_add_tokens(&self) -> TokenStream {
        let name = &self.name;
        let ty = payload_type(&self.func);
        quote! {
            builder.add_property({
                let uid = action_id.wrapping_next();
                ::qi::object::MetaProperty {
                    uid,
                    name: ::std::string::String::from(#name),
                    signature: <#ty as ::qi::value::Reflect>::signature(),
                }
            });
        }
    }
}

fn tagged_fn(item: &TraitItem, tag: &str) -> Result<Option<(TraitItemFn, String)>> {
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
    let name = parse_name_arg(attr, tag)?.unwrap_or_else(|| func.sig.ident.to_string());
    require_ref_self(func, tag)?;
    Ok(Some((func.clone(), name)))
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

fn parse_name_arg(attr: &Attribute, tag: &str) -> Result<Option<String>> {
    match &attr.meta {
        Meta::Path(_) => Ok(None),
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
                Lit::Str(s) => Ok(Some(s.value())),
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

fn meta_object_ident(trait_name: &Ident) -> Ident {
    Ident::new(
        &format!(
            "{}_META_OBJECT",
            trait_name.to_string().to_case(Case::UpperSnake)
        ),
        trait_name.span(),
    )
}

fn member_docs(func: &TraitItemFn) -> Option<String> {
    let docs: Vec<String> = func
        .attrs
        .iter()
        .filter_map(attribute_outer_doc)
        .map(|doc| {
            let value = doc.value();
            value.strip_prefix(' ').unwrap_or(&value).to_owned()
        })
        .collect();
    if docs.is_empty() {
        None
    } else {
        Some(docs.join("\n"))
    }
}

fn method_parameters(func: &TraitItemFn) -> TokenStream {
    let parameters = func.sig.inputs.iter().filter_map(|input| match input {
        FnArg::Typed(typed) => Some(typed),
        _ => None,
    });
    let statements = parameters.enumerate().map(|(index, typed)| {
        let ty = &typed.ty;
        let set_name = match &*typed.pat {
            Pat::Ident(pat) => {
                let name = pat.ident.to_string();
                Some(quote! {
                    method.parameter(#index).set_name(#name);
                })
            }
            _ => None,
        };
        quote! {
            #set_name
            method.parameter(#index).set_type(<#ty as ::qi::value::Reflect>::ty());
        }
    });
    quote! {
        #(#statements)*
    }
}

fn method_return_type(func: &TraitItemFn) -> TokenStream {
    match &func.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => {
            let ty = unwrap_result(ty);
            quote!(#ty)
        }
    }
}

fn payload_type(func: &TraitItemFn) -> TokenStream {
    match &func.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    }
}

fn unwrap_result(ty: &Type) -> &Type {
    let Type::Path(path) = ty else {
        return ty;
    };
    let Some(last) = path.path.segments.last() else {
        return ty;
    };
    if last.ident != "Result" {
        return ty;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return ty;
    };
    match args.args.first() {
        Some(syn::GenericArgument::Type(inner)) => inner,
        _ => ty,
    }
}
