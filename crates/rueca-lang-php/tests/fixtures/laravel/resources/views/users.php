<!doctype html>
<html>
<body>
<h1><?= htmlspecialchars($title) ?></h1>
<ul>
<?php foreach (\App\Models\User::active(10) as $user): ?>
    <li class="<?= $user->status->label() ?>"><?php echo e($user->name); ?></li>
<?php endforeach; ?>
</ul>
<?php include __DIR__ . '/partials/footer.php'; ?>
</body>
</html>
