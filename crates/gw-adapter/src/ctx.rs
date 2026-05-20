// Replaced in Task 8
pub struct AdapterCtx<'a>(std::marker::PhantomData<&'a ()>);
pub struct HeadersMut<'a>(std::marker::PhantomData<&'a ()>);
pub struct UrlMut<'a>(std::marker::PhantomData<&'a ()>);
pub trait HeadersAccessor {}
pub trait UrlAccessor {}
