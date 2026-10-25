<?php
function factorial(int $n): int
{
    return $n <= 1 ? 1 : $n * factorial($n - 1);
}

function isEven(int $n): bool
{
    return $n === 0 ? true : isOdd($n - 1);
}

function isOdd(int $n): bool
{
    return $n === 0 ? false : isEven($n - 1);
}
