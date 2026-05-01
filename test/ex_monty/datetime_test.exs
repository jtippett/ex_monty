defmodule ExMonty.DateTimeTest do
  use ExUnit.Case

  describe "date" do
    test "date.today() requires host handler" do
      # Without a sandbox handler, date.today() raises (no os handler).
      # Construction is what we exercise here; bare `date(y, m, d)` does not call out.
      assert {:ok, {:date, %{year: 2026, month: 5, day: 1}}, ""} =
               ExMonty.eval("from datetime import date\ndate(2026, 5, 1)")
    end

    test "round-trips date attributes" do
      code = """
      from datetime import date
      d = date(2024, 12, 31)
      (d.year, d.month, d.day)
      """

      assert {:ok, {2024, 12, 31}, ""} = ExMonty.eval(code)
    end
  end

  describe "datetime" do
    test "naive datetime encodes with nil offset and nil tz_name" do
      code = """
      from datetime import datetime
      datetime(2026, 5, 1, 12, 30, 45, 123456)
      """

      assert {:ok,
              {:datetime,
               %{
                 year: 2026,
                 month: 5,
                 day: 1,
                 hour: 12,
                 minute: 30,
                 second: 45,
                 microsecond: 123_456,
                 offset_seconds: nil,
                 tz_name: nil
               }}, ""} = ExMonty.eval(code)
    end

    test "aware datetime encodes with offset" do
      code = """
      from datetime import datetime, timezone
      datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
      """

      assert {:ok, {:datetime, %{offset_seconds: 0}}, ""} = ExMonty.eval(code)
    end
  end

  describe "timedelta" do
    test "encodes days, seconds, microseconds" do
      code = """
      from datetime import timedelta
      timedelta(days=2, seconds=10, microseconds=500)
      """

      assert {:ok, {:timedelta, %{days: 2, seconds: 10, microseconds: 500}}, ""} =
               ExMonty.eval(code)
    end
  end

  describe "timezone" do
    test "utc encodes with offset 0 and nil name" do
      code = """
      from datetime import timezone
      timezone.utc
      """

      assert {:ok, {:timezone, %{offset_seconds: 0, name: nil}}, ""} = ExMonty.eval(code)
    end
  end

  describe "host-provided date/datetime via os_call" do
    test "date.today() handler returns a real Python date" do
      code = """
      from datetime import date
      d = date.today()
      (d.year, d.month, d.day)
      """

      handler =
        fn _args, _kwargs ->
          {:ok, {:date, %{year: 2026, month: 5, day: 1}}}
        end

      assert {:ok, {2026, 5, 1}, ""} =
               ExMonty.Sandbox.run(code, os: %{date_today: handler})
    end

    test "datetime.now() handler returns a real Python datetime with tzinfo" do
      code = """
      from datetime import datetime, timezone
      dt = datetime.now(tz=timezone.utc)
      (dt.year, dt.hour, dt.tzinfo is not None)
      """

      handler =
        fn _args, _kwargs ->
          {:ok,
           {:datetime,
            %{
              year: 2026,
              month: 5,
              day: 1,
              hour: 14,
              minute: 30,
              second: 0,
              microsecond: 0,
              offset_seconds: 0,
              tz_name: nil
            }}}
        end

      assert {:ok, {2026, 14, true}, ""} =
               ExMonty.Sandbox.run(code, os: %{datetime_now: handler})
    end

    test "naive datetime.now() handler with nil offset" do
      code = """
      from datetime import datetime
      dt = datetime.now()
      (dt.year, dt.tzinfo is None)
      """

      handler =
        fn _args, _kwargs ->
          {:ok,
           {:datetime,
            %{
              year: 2026,
              month: 5,
              day: 1,
              hour: 12,
              minute: 0,
              second: 0,
              microsecond: 0,
              offset_seconds: nil,
              tz_name: nil
            }}}
        end

      assert {:ok, {2026, true}, ""} =
               ExMonty.Sandbox.run(code, os: %{datetime_now: handler})
    end
  end
end
