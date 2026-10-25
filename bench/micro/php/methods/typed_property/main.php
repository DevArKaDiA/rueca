<?php
class Mailer
{
    public function send(): void
    {
    }
}

class Notifier
{
    private Mailer $mailer;

    public function __construct()
    {
        $this->mailer = new Mailer();
    }

    public function notify(): void
    {
        $this->mailer->send();
    }
}
