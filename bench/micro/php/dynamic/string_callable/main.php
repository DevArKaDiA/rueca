<?php
function helper(): string
{
    return 'h';
}

function run(): string
{
    $f = 'helper';
    return $f();
}
