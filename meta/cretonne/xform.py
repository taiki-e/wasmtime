"""
Instruction transformations.
"""
from __future__ import absolute_import
from .ast import Def, Var, Apply


SRCCTX = 1
DSTCTX = 2


class Rtl(object):
    """
    Register Transfer Language list.

    An RTL object contains a list of register assignments in the form of `Def`
    objects and/or Apply objects for side-effecting instructions.

    An RTL list can represent both a source pattern to be matched, or a
    destination pattern to be inserted.
    """

    def __init__(self, *args):
        self.rtl = args

    def __iter__(self):
        return iter(self.rtl)


class XForm(object):
    """
    An instruction transformation consists of a source and destination pattern.

    Patterns are expressed in *register transfer language* as tuples of
    `ast.Def` or `ast.Expr` nodes.

    A legalization pattern must have a source pattern containing only a single
    instruction.

    >>> from .base import iconst, iadd, iadd_imm
    >>> a = Var('a')
    >>> c = Var('c')
    >>> v = Var('v')
    >>> x = Var('x')
    >>> XForm(
    ...     Rtl(c << iconst(v),
    ...         a << iadd(x, c)),
    ...     Rtl(a << iadd_imm(x, v)))
    XForm(inputs=[Var(v), Var(x)], defs=[Var(c, d=01), Var(a, d=11)],
      c << iconst(v)
      a << iadd(x, c)
    =>
      a << iadd_imm(x, v)
    )
    """

    def __init__(self, src, dst):
        self.src = src
        self.dst = dst
        # Variables that are inputs to the source pattern.
        self.inputs = list()
        # Variables defined in either src or dst.
        self.defs = list()

        # Rewrite variables in src and dst RTL lists to our own copies.
        # Map name -> private Var.
        symtab = dict()
        self._rewrite_rtl(src, symtab, SRCCTX)
        num_src_inputs = len(self.inputs)
        self._rewrite_rtl(dst, symtab, DSTCTX)

        # Check for inconsistently used inputs.
        for i in self.inputs:
            if i.defctx:
                raise AssertionError(
                        "'{}' used as both input and def".format(i))

        # Check for spurious inputs in dst.
        if len(self.inputs) > num_src_inputs:
            raise AssertionError(
                    "extra inputs in dst RTL: {}".format(
                        self.inputs[num_src_inputs:]))

    def __repr__(self):
        s = "XForm(inputs={}, defs={},\n  ".format(self.inputs, self.defs)
        s += '\n  '.join(str(n) for n in self.src)
        s += '\n=>\n  '
        s += '\n  '.join(str(n) for n in self.dst)
        s += '\n)'
        return s

    def _rewrite_rtl(self, rtl, symtab, context):
        for line in rtl:
            if isinstance(line, Def):
                line.defs = tuple(
                        self._rewrite_defs(line.defs, symtab, context))
                expr = line.expr
            else:
                expr = line
            self._rewrite_expr(expr, symtab, context)

    def _rewrite_expr(self, expr, symtab, context):
        """
        Find all uses of variables in `expr` and replace them with our own
        local symbols.
        """

        # Accept a whole expression tree.
        stack = [expr]
        while len(stack) > 0:
            expr = stack.pop()
            expr.args = tuple(
                    self._rewrite_uses(expr, stack, symtab, context))

    def _rewrite_defs(self, defs, symtab, context):
        """
        Given a tuple of symbols defined in a Def, rewrite them to local
        symbols. Yield the new locals.
        """
        for sym in defs:
            name = str(sym)
            if name in symtab:
                var = symtab[name]
                if var.defctx & context:
                    raise AssertionError("'{}' multiply defined".format(name))
            else:
                var = Var(name)
                symtab[name] = var
                self.defs.append(var)
            var.defctx |= context
            yield var

    def _rewrite_uses(self, expr, stack, symtab, context):
        """
        Given an `Apply` expr, rewrite all uses in its arguments to local
        variables. Yield a sequence of new arguments.

        Append any `Apply` arguments to `stack`.
        """
        for arg, operand in zip(expr.args, expr.inst.ins):
            # Nested instructions are allowed. Visit recursively.
            if isinstance(arg, Apply):
                stack.push(arg)
                yield arg
                continue
            if not isinstance(arg, Var):
                assert not operand.is_value(), "Value arg must be `Var`"
                yield arg
                continue
            # This is supposed to be a symbolic value reference.
            name = str(arg)
            if name in symtab:
                var = symtab[name]
                # The variable must be used consistenty as a def or input.
                if var.defctx and (var.defctx & context) == 0:
                    raise AssertionError(
                            "'{}' used as both input and def"
                            .format(name))
            else:
                # First time use of variable.
                var = Var(name)
                symtab[name] = var
                self.inputs.append(var)
            yield var


class XFormGroup(object):
    """
    A group of related transformations.
    """

    def __init__(self):
        self.xforms = list()

    def legalize(self, src, dst):
        """
        Add a legalization pattern to this group.

        :param src: Single `Def` or `Apply` to be legalized.
        :param dst: `Rtl` list of replacement instructions.
        """
        self.xforms.append(XForm(Rtl(src), dst))
