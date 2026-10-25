<?php
function double(int $x): int
{
    return $x * 2;
}

function doubled(array $xs): array
{
    return array_map(fn ($x) => double($x), $xs);
}
