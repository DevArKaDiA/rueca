<?php
function notify(Mailer $mailer): void
{
    $mailer->send('a@example.com');
}
