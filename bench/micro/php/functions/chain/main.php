<?php
function first(int $x): int
{
    return second($x + 1);
}

function second(int $x): int
{
    return third($x * 2);
}

function third(int $x): int
{
    return fourth($x) - 1;
}

function fourth(int $x): int
{
    return $x;
}

echo first(1);
