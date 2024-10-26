use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse::Parse, parse::ParseStream, parse_macro_input, Ident, Token};

struct TypeParams {
    idents: Vec<Ident>,
}

impl Parse for TypeParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut idents = Vec::new();

        while !input.is_empty() {
            idents.push(input.parse()?);
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(TypeParams { idents })
    }
}

#[proc_macro]
pub fn impl_from_tuple(input: TokenStream) -> TokenStream {
    let TypeParams { idents } = parse_macro_input!(input as TypeParams);
    let n = idents.len();

    let idents: Vec<(Ident, Ident)> = idents
        .into_iter()
        .map(|ident| (ident.clone(), format_ident!("{}Str", ident)))
        .collect();

    // Generate generic bounds
    let generic_bounds = idents.iter().flat_map(|(id, str_id)| {
        [
            quote! { #id: Into<DataValue> },
            quote! { #str_id: Into<String> },
        ]
    });

    // Generate tuple type
    let tuple_type = idents
        .iter()
        .map(|(ident, str_id)| quote! { (#str_id, #ident) });
    let tt2 = tuple_type.clone();
    // Generate data insertions
    let data_insertions = (0..n).map(|i| {
        let idx = syn::Index::from(i);
        quote! {
            data.insert(value.#idx.0.into(), value.#idx.1.into());
        }
    });

    let expanded = quote! {
        impl<#(#generic_bounds),*> From<(#(#tuple_type),*,)> for LogData {
            fn from(value: (#(#tt2),*,)) -> Self {
                let mut data = HashMap::new();
                #(#data_insertions)*
                data.into()
            }
        }
    };

    expanded.into()
}
