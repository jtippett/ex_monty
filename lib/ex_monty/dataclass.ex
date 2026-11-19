defmodule ExMonty.Dataclass do
  @moduledoc """
  Represents a Python class instance crossing the sandbox boundary.

  Pass one in (as an input or a callback result) and the sandbox sees an
  instance of a dataclass named `:name` with the given fields. Instances
  created *inside* the sandbox — of a `@dataclass` or of any other class —
  come back out as this struct too, with their instance attributes as fields.

  ## Fields

    * `:name` - the class name
    * `:fields` - map of field name strings to values
    * `:field_names` - declaration-order field names (derived from `fields` keys when `nil`)
    * `:type_id` - class identity, a UUID string. Instances with the same
      `type_id` are instances of one class inside the sandbox. When `nil` it is
      derived from `:name`, so same-named dataclasses share a class. (An
      integer, as used before the monty v1 update, is still accepted and mixed into the
      derived id.)
    * `:id` - instance identity, a UUID string. Set on every value that comes
      out of the sandbox; when `nil` on the way in, a fresh one is generated.
      Handing a struct back with its `:id` intact refers to the *same* object:
      a still-alive sandbox instance resolves to the original (methods and
      all), and a host object keeps its identity for method calls.
    * `:origin` - `:host` for an object the host supplied, `:sandbox` for an
      instance of a class defined by the Python code. Set on every value that
      comes out; leave it at the default when building a struct yourself.
      Only `:host` objects can route method calls to the host.
    * `:frozen` - always `false`. Upstream removed frozen enforcement at the
      boundary, so `frozen: true` is rejected rather than silently ignored.

  ## Semantics worth knowing

  The sandbox works on its **own copy** of the fields. Attribute writes made
  by Python code are visible to that code and in the value it returns, but a
  `{:method_call, call, snapshot, output}` hands the host the receiver as the
  host last sent it (`hd(call.args)`), not with the sandbox's writes applied.

  "Sent" includes sending it *back*: a host function that returns an object
  it received from the sandbox has sent whatever the sandbox put in its
  fields. Treat the fields of a method-call receiver as untrusted input unless
  every host function that returns such objects builds them itself.

  A `:sandbox` instance handed back while the original is still alive *is*
  the original. If the original is gone (for example it was a temporary, as
  in `p = keep(P())`), the sandbox gets a plain data copy: the fields are
  there, the class's methods are not.
  """

  @type t :: %__MODULE__{
          name: String.t(),
          fields: map(),
          field_names: [String.t()] | nil,
          type_id: String.t() | non_neg_integer() | nil,
          id: String.t() | nil,
          origin: :host | :sandbox,
          frozen: boolean()
        }

  defstruct [:name, :fields, :field_names, :type_id, :id, origin: :host, frozen: false]
end
