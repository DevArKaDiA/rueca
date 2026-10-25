<?php
function wrap(string $s): string
{
    return "[$s]";
}

function shout(string $s): string
{
    return $s . '!';
}

function word(): string
{
    return 'hi';
}

function render(): string
{
    return wrap(shout(word()));
}
