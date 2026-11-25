using static App.Util.Strings;

namespace App
{
    public class Greeter
    {
        public string Greet(string name)
        {
            return Shout(name);
        }
    }
}

namespace App.Util
{
    public static class Strings
    {
        public static string Shout(string s)
        {
            return s.ToUpper();
        }
    }
}
