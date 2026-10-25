<?php
namespace App;

use Lib\Support\Str;

function normalize(string $s): string
{
    return Str::lower($s);
}
