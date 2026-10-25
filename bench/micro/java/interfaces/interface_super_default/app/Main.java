package app;

interface Walker {
    default String move() {
        return "walk";
    }
}

class Robot implements Walker {
    @Override
    public String move() {
        return "robot " + Walker.super.move();
    }
}
