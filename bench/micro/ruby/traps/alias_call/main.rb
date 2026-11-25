class Doc
  def render
    1
  end
  alias_method :to_html, :render

  def show
    to_html
  end
end
