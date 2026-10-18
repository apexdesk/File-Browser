//! Hilos en el navegador, que no los tiene: lo que se da a un hilo nuevo se hace en el momento, en
//! este mismo, y el resultado queda en su manejador. Un hilo pensado para vivir tanto como el
//! programa no puede arrancarse aquí.
use std::io;
use std::marker::PhantomData;
use std::time::Duration;

pub use std::thread::{Result, Thread, ThreadId, current, panicking, yield_now};

pub struct JoinHandle<T> {
    result: Option<T>,
}

impl<T> JoinHandle<T> {
    pub fn join(mut self) -> Result<T> {
        Ok(self.result.take().expect("un solo join"))
    }

    pub fn is_finished(&self) -> bool {
        true
    }
}

pub fn spawn<F: FnOnce() -> T, T>(f: F) -> JoinHandle<T> {
    JoinHandle { result: Some(f()) }
}

/// Esperar pararía la página: los fotogramas del navegador siguen su curso.
pub fn sleep(_d: Duration) {}

pub fn available_parallelism() -> io::Result<std::num::NonZeroUsize> {
    Ok(std::num::NonZeroUsize::MIN)
}

#[derive(Debug, Default)]
pub struct Builder;

impl Builder {
    pub fn new() -> Builder {
        Builder
    }

    pub fn name(self, _name: String) -> Builder {
        self
    }

    pub fn stack_size(self, _size: usize) -> Builder {
        self
    }

    pub fn spawn<F: FnOnce() -> T, T>(self, f: F) -> io::Result<JoinHandle<T>> {
        Ok(spawn(f))
    }
}

pub struct Scope<'scope, 'env: 'scope> {
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}

pub struct ScopedJoinHandle<'scope, T> {
    result: Option<T>,
    scope: PhantomData<&'scope ()>,
}

impl<T> ScopedJoinHandle<'_, T> {
    pub fn join(mut self) -> Result<T> {
        Ok(self.result.take().expect("un solo join"))
    }
}

impl<'scope> Scope<'scope, '_> {
    pub fn spawn<F: FnOnce() -> T + 'scope, T: 'scope>(&'scope self, f: F) -> ScopedJoinHandle<'scope, T> {
        ScopedJoinHandle { result: Some(f()), scope: PhantomData }
    }
}

pub fn scope<'env, F, T>(f: F) -> T
where
    F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
{
    f(&Scope { scope: PhantomData, env: PhantomData })
}
