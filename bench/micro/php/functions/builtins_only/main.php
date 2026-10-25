<?php
function label(string $s): string
{
    return strtoupper(trim($s)) . strlen($s);
}

function count_items(array $xs): int
{
    return count(array_filter($xs));
}

function main(): void
{
    echo label(' x ');
    echo count_items([1, 0]);
}
