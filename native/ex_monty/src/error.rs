use monty::{MontyException, ResourceError};
use rustler::{Encoder, Env, Term};

/// Convert a MontyException to a Rustler error with a descriptive term.
pub fn monty_exception_to_rustler_error(exc: MontyException) -> rustler::Error {
    rustler::Error::Term(Box::new(ExceptionWrapper(exc)))
}

/// Convert a ResourceError to a Rustler error with a descriptive term.
#[allow(dead_code)]
pub fn resource_error_to_rustler_error(err: ResourceError) -> rustler::Error {
    rustler::Error::Term(Box::new(ResourceErrorWrapper(err)))
}

struct ExceptionWrapper(MontyException);

impl Encoder for ExceptionWrapper {
    fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
        encode_monty_exception(env, &self.0)
    }
}

#[allow(dead_code)]
struct ResourceErrorWrapper(ResourceError);

impl Encoder for ResourceErrorWrapper {
    fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
        encode_resource_error(env, &self.0)
    }
}

/// Encode a MontyException as an Elixir-friendly term:
/// %ExMonty.Exception{type: atom, message: string | nil, traceback: [frame]}
pub fn encode_monty_exception<'a>(env: Env<'a>, exc: &MontyException) -> Term<'a> {
    let exc_type_str = exc.exc_type().to_string();
    let exc_type_atom = rustler::types::atom::Atom::from_str(env, &snake_case(&exc_type_str))
        .unwrap()
        .encode(env);

    let message = match exc.message() {
        Some(msg) => msg.encode(env),
        None => rustler::types::atom::nil().encode(env),
    };

    let traceback: Vec<Term> = exc
        .traceback()
        .iter()
        .map(|frame| encode_stack_frame(env, frame))
        .collect();

    let struct_atom =
        rustler::types::atom::Atom::from_str(env, "Elixir.ExMonty.Exception").unwrap();

    rustler::types::map::map_new(env)
        .map_put(
            rustler::types::atom::Atom::from_str(env, "__struct__")
                .unwrap()
                .encode(env),
            struct_atom.encode(env),
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "type")
                .unwrap()
                .encode(env),
            exc_type_atom,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "message")
                .unwrap()
                .encode(env),
            message,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "traceback")
                .unwrap()
                .encode(env),
            traceback.encode(env),
        )
        .unwrap()
}

fn encode_stack_frame<'a>(env: Env<'a>, frame: &monty::StackFrame) -> Term<'a> {
    let struct_atom =
        rustler::types::atom::Atom::from_str(env, "Elixir.ExMonty.StackFrame").unwrap();

    let filename = frame.filename.encode(env);
    let line = frame.start.line.encode(env);
    let column = frame.start.column.encode(env);
    let end_line = frame.end.line.encode(env);
    let end_column = frame.end.column.encode(env);
    let frame_name = match &frame.frame_name {
        Some(name) => name.encode(env),
        None => rustler::types::atom::nil().encode(env),
    };

    rustler::types::map::map_new(env)
        .map_put(
            rustler::types::atom::Atom::from_str(env, "__struct__")
                .unwrap()
                .encode(env),
            struct_atom.encode(env),
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "filename")
                .unwrap()
                .encode(env),
            filename,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "line")
                .unwrap()
                .encode(env),
            line,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "column")
                .unwrap()
                .encode(env),
            column,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "end_line")
                .unwrap()
                .encode(env),
            end_line,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "end_column")
                .unwrap()
                .encode(env),
            end_column,
        )
        .unwrap()
        .map_put(
            rustler::types::atom::Atom::from_str(env, "name")
                .unwrap()
                .encode(env),
            frame_name,
        )
        .unwrap()
}

#[allow(dead_code)]
fn encode_resource_error<'a>(env: Env<'a>, err: &ResourceError) -> Term<'a> {
    match err {
        ResourceError::Allocation { limit, count } => {
            let tag = rustler::types::atom::Atom::from_str(env, "allocation_limit").unwrap();
            rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), limit.encode(env), count.encode(env)],
            )
        }
        ResourceError::Time { limit, elapsed } => {
            let tag = rustler::types::atom::Atom::from_str(env, "time_limit").unwrap();
            let limit_secs = limit.as_secs_f64();
            let elapsed_secs = elapsed.as_secs_f64();
            rustler::types::tuple::make_tuple(
                env,
                &[
                    tag.encode(env),
                    limit_secs.encode(env),
                    elapsed_secs.encode(env),
                ],
            )
        }
        ResourceError::Memory { limit, used } => {
            let tag = rustler::types::atom::Atom::from_str(env, "memory_limit").unwrap();
            rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), limit.encode(env), used.encode(env)],
            )
        }
        ResourceError::Recursion { limit, depth } => {
            let tag = rustler::types::atom::Atom::from_str(env, "recursion_limit").unwrap();
            rustler::types::tuple::make_tuple(
                env,
                &[tag.encode(env), limit.encode(env), depth.encode(env)],
            )
        }
        ResourceError::Exception(exc) => encode_monty_exception(env, exc),
    }
}

/// Convert PascalCase to snake_case for atom names
fn snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}
