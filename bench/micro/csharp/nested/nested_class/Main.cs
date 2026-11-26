namespace App;

public class Outer
{
    public class Inner
    {
        public void Act()
        {
        }
    }

    public void Use()
    {
        new Inner().Act();
    }
}
