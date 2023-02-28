use proc_macro::TokenStream;
use quote::quote;
use syn;

#[proc_macro_derive(TransferHandles)]
pub fn transfer_hadnles_derive(input: TokenStream) -> TokenStream {
    // Construct a representation of Rust code as a syntax tree
    // that we can manipulate
    let ast = syn::parse(input).unwrap();

    // Build the trait implementation
    impl_hello_macro(&ast)
}

fn impl_hello_macro(ast: &syn::DeriveInput) -> TokenStream {
    let name = &ast.ident;

    // let fields = match &ast.data {
    //     syn::Data::Struct(s) => {
    //         s.
    //     },
    //     syn::Data::Enum(_) => todo!(),
    //     syn::Data::Union(_) => todo!(),
    // };


    let gen = quote! {
        impl TransferHandles for #name {
            fn move_handles<Transport: HandlesTransport>(&self, _transport: Transport) -> Result<(), Transport::Error> {
                Ok(())
            }

            fn receive_handles<Transport: HandlesTransport>(&mut self, _transport: Transport) -> Result<(), Transport::Error> {
                Ok(())
            }
        }
    };
    gen.into()
}
