//! Dependency injection with per-request memoisation.

use crate::error::ApiError;
use crate::extract::FromRequest;
use crate::operation::{OperationContext, OperationInput};
use crate::request::Request;
use std::any::TypeId;
use std::future::Future;
use std::ops::Deref;

/// A value computed from the request, resolved at most once per request.
///
/// This is FastAPI's `Depends` with the sharp edges filed off. Dependencies
/// compose — one can extract others — and a dependency needed by three
/// different sub-dependencies still runs a single time, so an auth lookup does
/// not turn into three database round-trips.
///
/// ```ignore
/// #[derive(Clone)]
/// struct CurrentUser(String);
///
/// impl Dependency for CurrentUser {
///     async fn resolve(req: &mut Request) -> Result<Self, ApiError> {
///         let Bearer(token) = Bearer::from_request(req).await?;
///         let State(db) = State::<Db>::from_request(req).await?;
///         db.user_for_token(&token)
///             .await
///             .ok_or_else(|| ApiError::unauthorized("Unknown token."))
///     }
///
///     fn describe(ctx: &mut OperationContext<'_>) {
///         // Inherit the documentation of whatever this depends on.
///         <Bearer as OperationInput>::describe(ctx);
///     }
/// }
/// ```
pub trait Dependency: Clone + Send + Sync + Sized + 'static {
    /// Computes the value. May itself use extractors and other dependencies.
    fn resolve(req: &mut Request) -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Contributes to the operation description — security requirements,
    /// headers a dependency reads, error responses it can produce.
    ///
    /// Overriding this is what keeps a dependency from being invisible in the
    /// docs, which is a genuine failure mode in FastAPI.
    fn describe(ctx: &mut OperationContext<'_>) {
        let _ = ctx;
    }
}

/// Wrapper that resolves a [`Dependency`] as a handler argument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Depends<T>(pub T);

impl<T> Depends<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Depends<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Dependency> FromRequest for Depends<T> {
    async fn from_request(req: &mut Request) -> Result<Self, ApiError> {
        if let Some(cached) = req
            .cache
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref::<T>())
        {
            return Ok(Depends(cached.clone()));
        }

        let value = T::resolve(req).await?;
        req.cache.insert(TypeId::of::<T>(), Box::new(value.clone()));
        Ok(Depends(value))
    }
}

impl<T: Dependency> OperationInput for Depends<T> {
    fn describe(ctx: &mut OperationContext<'_>) {
        T::describe(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{Bearer, State};
    use crate::testing::test_request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Each test gets its own dependency type and counter: the cache is keyed
    /// by `TypeId`, and tests run in parallel, so a shared counter would make
    /// them interfere.
    macro_rules! counting_dependency {
        ($name:ident, $counter:ident) => {
            static $counter: AtomicUsize = AtomicUsize::new(0);

            #[derive(Clone)]
            struct $name(#[allow(dead_code)] usize);

            impl Dependency for $name {
                async fn resolve(_req: &mut Request) -> Result<Self, ApiError> {
                    Ok($name($counter.fetch_add(1, Ordering::SeqCst)))
                }
            }
        };
    }

    #[tokio::test]
    async fn a_dependency_resolves_once_per_request() {
        counting_dependency!(OncePerRequest, ONCE_PER_REQUEST);

        let mut req = test_request().build();
        let a = Depends::<OncePerRequest>::from_request(&mut req)
            .await
            .unwrap();
        let b = Depends::<OncePerRequest>::from_request(&mut req)
            .await
            .unwrap();
        let c = Depends::<OncePerRequest>::from_request(&mut req)
            .await
            .unwrap();

        assert_eq!(ONCE_PER_REQUEST.load(Ordering::SeqCst), 1);
        assert_eq!((a.0 .0, b.0 .0, c.0 .0), (0, 0, 0));
    }

    #[tokio::test]
    async fn a_fresh_request_resolves_again() {
        counting_dependency!(PerRequest, PER_REQUEST);

        let mut first = test_request().build();
        let mut second = test_request().build();
        Depends::<PerRequest>::from_request(&mut first)
            .await
            .unwrap();
        Depends::<PerRequest>::from_request(&mut second)
            .await
            .unwrap();
        assert_eq!(PER_REQUEST.load(Ordering::SeqCst), 2);
    }

    #[derive(Clone, Debug, PartialEq)]
    struct CurrentUser(String);

    struct Tokens(Vec<(&'static str, &'static str)>);

    impl Dependency for CurrentUser {
        async fn resolve(req: &mut Request) -> Result<Self, ApiError> {
            let Bearer(token) = Bearer::from_request(req).await?;
            let State(tokens) = State::<Tokens>::from_request(req).await?;
            tokens
                .0
                .iter()
                .find(|(t, _)| *t == token)
                .map(|(_, user)| CurrentUser((*user).to_owned()))
                .ok_or_else(|| ApiError::unauthorized("Unknown token."))
        }

        fn describe(ctx: &mut OperationContext<'_>) {
            <Bearer as OperationInput>::describe(ctx);
        }
    }

    #[tokio::test]
    async fn dependencies_compose_over_extractors_and_state() {
        let mut state = crate::state::StateMap::new();
        state.insert(Tokens(vec![("good", "ada")]));

        let mut req = test_request()
            .header("authorization", "Bearer good")
            .state(Arc::new(state))
            .build();

        let user = Depends::<CurrentUser>::from_request(&mut req)
            .await
            .unwrap();
        assert_eq!(user.0, CurrentUser("ada".into()));
    }

    #[tokio::test]
    async fn a_failing_dependency_propagates_its_own_status() {
        let mut state = crate::state::StateMap::new();
        state.insert(Tokens(vec![("good", "ada")]));
        let mut req = test_request()
            .header("authorization", "Bearer bad")
            .state(Arc::new(state))
            .build();

        let err = Depends::<CurrentUser>::from_request(&mut req)
            .await
            .unwrap_err();
        assert_eq!(err.status(), http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_dependency_contributes_its_security_requirement() {
        let mut generator = velo_openapi::SchemaGenerator::new();
        let mut operation = velo_openapi::Operation::default();
        let mut ctx = OperationContext {
            generator: &mut generator,
            operation: &mut operation,
            path: "/me",
            method: "GET",
        };
        Depends::<CurrentUser>::describe(&mut ctx);
        assert_eq!(operation.security.as_ref().unwrap().len(), 1);
        assert!(operation.responses.contains_key("401"));
    }
}
