<?php
function inner(): void
{
}

function outer(): void
{
    $g = function () {
        inner();
    };
    $g();
}
