<?php
// SPDX-License-Identifier: Apache-2.0
//
// rueca-bench PHP call-edge extractor (S3 gold for PHP, RFC 0002 §4.2: scip-php
// is not viable). A PHPStan collector records every call site with the target
// PHPStan resolves from its type inference; a rule on CollectedDataNode dumps
// them as JSON to $RUECA_OUT. Callers are NOT decided here: the bench maps each
// call site (file + line) to the innermost tree-sitter callable, exactly like
// the SCIP-derived gold (bench/src/rueca_bench/phpstan.py).
//
// Resolved: $this->m(), $obj->m() / $obj?->m() on any inferred receiver type
// (typed parameters, properties, promoted constructor properties, return
// types, `new`, phpdoc), self:: / static:: / parent:: / Class::m(), new C
// (-> C::__construct, inherited constructors included), named functions
// (namespace fallback included), $obj() (-> __invoke) and constant string
// callables `$f = 'fn'; $f()`. Not resolved: call_user_func / array_map
// callables, __call / __callStatic, variable method names.

declare(strict_types=1);

namespace RuecaBench;

use PhpParser\Node;
use PhpParser\Node\Expr;
use PhpParser\Node\Identifier;
use PhpParser\Node\Name;
use PHPStan\Analyser\Scope;
use PHPStan\Collectors\Collector;
use PHPStan\Node\CollectedDataNode;
use PHPStan\Reflection\ClassReflection;
use PHPStan\Reflection\ReflectionProvider;
use PHPStan\Rules\Rule;

/**
 * @implements Collector<Expr, array{line: int, file: string, kind: string, targets: list<array{kind: string, name: string, file: ?string, line: ?int}>}>
 */
final class CallCollector implements Collector
{
    public function __construct(private ReflectionProvider $reflectionProvider)
    {
    }

    public function getNodeType(): string
    {
        return Expr::class;
    }

    public function processNode(Node $node, Scope $scope)
    {
        try {
            [$kind, $targets] = $this->targets($node, $scope);
        } catch (\Throwable $e) {
            return null;
        }
        if ($kind === null) {
            return null;
        }
        return [
            'line' => $node->getStartLine(),
            'file' => $scope->getFile(),
            'kind' => $kind,
            'targets' => array_values(array_filter($targets)),
        ];
    }

    /** @return array{0: ?string, 1: list<?array{kind: string, name: string, file: ?string, line: ?int}>} */
    private function targets(Node $node, Scope $scope): array
    {
        if ($node instanceof Expr\MethodCall || $node instanceof Expr\NullsafeMethodCall) {
            if (!$node->name instanceof Identifier) {
                return ['method', []];
            }
            $out = [];
            foreach ($scope->getType($node->var)->getObjectClassReflections() as $cr) {
                $out[] = $this->method($cr, $node->name->toString());
            }
            return ['method', $out];
        }
        if ($node instanceof Expr\StaticCall) {
            if (!$node->name instanceof Identifier) {
                return ['static', []];
            }
            $out = [];
            foreach ($this->classesOf($node->class, $scope) as $cr) {
                $out[] = $this->method($cr, $node->name->toString());
            }
            return ['static', $out];
        }
        if ($node instanceof Expr\New_) {
            $out = [];
            if (!$node->class instanceof Node\Stmt\Class_) {
                foreach ($this->classesOf($node->class, $scope) as $cr) {
                    $out[] = $this->method($cr, '__construct');
                }
            }
            return ['new', $out];
        }
        if ($node instanceof Expr\FuncCall) {
            if ($node->name instanceof Name) {
                return ['function', [$this->func($node->name, $scope)]];
            }
            $out = [];
            $type = $scope->getType($node->name);
            foreach ($type->getConstantStrings() as $s) {
                $out[] = $this->func(new Name\FullyQualified(ltrim($s->getValue(), '\\')), $scope);
            }
            foreach ($type->getObjectClassReflections() as $cr) {
                $out[] = $this->method($cr, '__invoke');
            }
            return ['dynamic', $out];
        }
        return [null, []];
    }

    /** @return list<ClassReflection> */
    private function classesOf(Node $class, Scope $scope): array
    {
        if ($class instanceof Name) {
            $name = $scope->resolveName($class);
            return $this->reflectionProvider->hasClass($name) ? [$this->reflectionProvider->getClass($name)] : [];
        }
        if ($class instanceof Expr) {
            $type = $scope->getType($class);
            $out = $type->getObjectClassReflections();
            foreach ($type->getConstantStrings() as $s) {
                if ($this->reflectionProvider->hasClass($s->getValue())) {
                    $out[] = $this->reflectionProvider->getClass($s->getValue());
                }
            }
            return $out;
        }
        return [];
    }

    /** @return ?array{kind: string, name: string, file: ?string, line: ?int} */
    private function method(ClassReflection $cr, string $name): ?array
    {
        $native = $cr->getNativeReflection();
        if (!$native->hasMethod($name)) {
            return null;
        }
        $m = $native->getMethod($name);
        if ($m->isInternal()) {
            return null;
        }
        $file = $m->getFileName();
        return [
            'kind' => 'method',
            'name' => $m->getDeclaringClass()->getName() . '::' . $m->getName(),
            'file' => $file === false ? null : $file,
            'line' => $m->getStartLine() ?: null,
        ];
    }

    /** @return ?array{kind: string, name: string, file: ?string, line: ?int} */
    private function func(Name $name, Scope $scope): ?array
    {
        if (!$this->reflectionProvider->hasFunction($name, $scope)) {
            return null;
        }
        $f = $this->reflectionProvider->getFunction($name, $scope);
        if ($f->isBuiltin()) {
            return null;
        }
        $file = $f->getFileName();
        return ['kind' => 'function', 'name' => $f->getName(), 'file' => $file, 'line' => null];
    }
}

/**
 * @implements Rule<CollectedDataNode>
 */
final class DumpRule implements Rule
{
    public function getNodeType(): string
    {
        return CollectedDataNode::class;
    }

    public function processNode(Node $node, Scope $scope): array
    {
        $out = getenv('RUECA_OUT') ?: '/out/calls.json';
        $data = $node->get(CallCollector::class);
        ksort($data);
        file_put_contents($out, json_encode($data, JSON_UNESCAPED_SLASHES | JSON_INVALID_UTF8_SUBSTITUTE));
        return [];
    }
}
