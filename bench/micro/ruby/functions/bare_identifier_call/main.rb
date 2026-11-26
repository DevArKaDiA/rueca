class Report
  def title
    "t"
  end

  def render
    text = title
    text + footer
  end

  def footer
    "f"
  end
end
