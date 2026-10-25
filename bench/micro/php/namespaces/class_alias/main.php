<?php
namespace App;

use Lib\Logger as L;

function report(): void
{
    $l = new L();
    $l->log('x');
}
