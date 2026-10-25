<?php
namespace App;

use Lib\{Foo, Bar};

function build(): void
{
    Foo::make();
    new Bar();
}
