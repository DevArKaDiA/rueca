<?php
function a(): void
{
    b();
}

function b(): void
{
}

function c(): void
{
    a();
    b();
}

c();
