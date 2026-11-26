namespace App;

public class Widget
{
    public virtual void Draw()
    {
    }
}

public class Button : Widget
{
    public override void Draw()
    {
        base.Draw();
    }
}
