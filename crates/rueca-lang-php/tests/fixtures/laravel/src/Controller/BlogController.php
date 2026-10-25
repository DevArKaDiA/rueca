<?php

namespace Acme\Blog\Controller;

use Acme\Blog\Entity\Post;
use Symfony\Bundle\FrameworkBundle\Controller\AbstractController;
use Symfony\Component\HttpFoundation\Response;
use Symfony\Component\Routing\Attribute\Route;
use Symfony\Component\Security\Http\Attribute\IsGranted;

#[Route('/blog', name: 'blog_')]
final class BlogController extends AbstractController
{
    #[Route('/{slug}', name: 'show', methods: ['GET'])]
    #[IsGranted('ROLE_USER')]
    public function show(Post $post): Response
    {
        return $this->render('blog/show.html.twig', ['post' => $post->publish()]);
    }
}
